use egui::TextBuffer;

use super::*;

impl HomePage {
    /// Shared view-mode selection (`Source` / `Preview` / `Split`). Returns the
    /// mode that was just selected, if any. Used by the editor's context menu
    /// and the preview's right-click popup.
    fn mode_menu_items(ui: &mut egui::Ui, view: &View) -> Option<ViewMode> {
        // Plain labels: decorative glyphs (e.g. "✎") are not covered by the
        // installed fonts and render as tofu.
        if ui
            .selectable_label(view.mode == ViewMode::Source, "Source")
            .clicked()
        {
            Some(ViewMode::Source)
        } else if ui
            .selectable_label(view.mode == ViewMode::Preview, "Preview")
            .clicked()
        {
            Some(ViewMode::Preview)
        } else {
            None
        }
    }

    /// Applies a mode selected from a menu, focusing the editor when entering
    /// an editing mode.
    fn apply_view_mode(view: &mut View, mode: ViewMode) {
        view.mode = mode;
        if mode.is_editing() {
            view.focus_edit = true;
        }
    }

    fn show_content_context_menu(
        ui: &mut egui::Ui,
        content: &str,
        selection: Option<egui::text::CCursorRange>,
        view: &mut View,
    ) {
        if let Some(selection) = selection {
            let selected = content
                .char_range(selection.as_sorted_char_range())
                .to_owned();
            if !selected.is_empty() && ui.button("Copy").clicked() {
                ui.copy_text(selected);
                ui.close();
            } else if selected.is_empty() && ui.button("Copy All").clicked() {
                ui.copy_text(content.to_owned());
                ui.close();
            }
        } else if ui.button("Copy All").clicked() {
            ui.copy_text(content.to_owned());
            ui.close();
        }

        ui.separator();
        if let Some(mode) = Self::mode_menu_items(ui, view) {
            Self::apply_view_mode(view, mode);
            ui.close();
        }
    }

    /// Renders the raw-markdown source editor (used in `Source` and `Split`
    /// modes). `Esc` switches back to the preview.
    fn render_snippet_content(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
        pane_rect: egui::Rect,
    ) {
        // Global per-view id: independent of the parent `Ui` (columns created
        // by `ui.columns` share one stable id, which would otherwise make the
        // editor state ambiguous in Split mode). Also preserves cursor/IME
        // state across mode switches.
        let text_edit_id = egui::Id::new(("snippet-content", view.id));
        let saved_selection = egui::TextEdit::load_state(ui.ctx(), text_edit_id)
            .and_then(|state| state.cursor.char_range());
        let secondary_pressed = ui.input(|input| input.pointer.secondary_pressed());

        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            view.mode = ViewMode::Preview;
            ui.memory_mut(|memory| memory.surrender_focus(text_edit_id));
            ui.input_mut(|input| {
                input.consume_key(input.modifiers, egui::Key::Escape);
            });
        }

        let mut output = egui::TextEdit::multiline(&mut snippet.content)
            .id(text_edit_id)
            .font(egui::FontId::proportional(18.0))
            .desired_width(f32::INFINITY)
            .frame(egui::Frame::NONE)
            .show(ui);

        let saved_selection = saved_selection.map(|mut range| {
            range.primary = output.galley.clamp_cursor(&range.primary);
            range.secondary = output.galley.clamp_cursor(&range.secondary);
            range
        });
        if secondary_pressed && output.response.contains_pointer() {
            output.state.cursor.set_char_range(saved_selection);
            output.state.store(ui.ctx(), output.response.id);
        }
        let selection = if secondary_pressed && output.response.contains_pointer() {
            saved_selection
        } else {
            output.cursor_range
        };

        if output.response.changed() {
            // Quasi-real-time: the preview pane (Split mode) re-renders from
            // the same buffer next frame; save immediately for crash safety.
            ui.ctx().request_repaint();
            let _ = store.save(snippet);
        }
        if view.focus_edit {
            output.response.request_focus();
            view.focus_edit = false;
        }

        output.response.context_menu(|ui| {
            Self::show_content_context_menu(ui, &snippet.content, selection, view);
        });

        // Right-click on the empty pane area (below the editor text) also opens
        // the view-mode menu; the editor's own context menu covers the text.
        let secondary_clicked = ui.input(|input| input.pointer.secondary_clicked());
        if secondary_clicked
            && let Some(pos) = ui.input(|input| input.pointer.interact_pos())
            && pane_rect.contains(pos)
            && !output.response.rect.contains(pos)
        {
            view.mode_menu = Some(pos);
        }
    }

    /// Renders the CommonMark preview. Local `{title}--{id}.md` links are
    /// hooked so a click returns the target [`EntityId`] to open; a right-click
    /// anywhere in the visible pane (content or the empty area below it) opens
    /// the view-mode menu ([`View::mode_menu`]).
    fn render_markdown_preview(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        pane_rect: egui::Rect,
    ) -> Option<EntityId> {
        // Register hooks *before* `show`: `prepare_show` resets hook values at
        // the start of the frame, then `Link::end` marks a hook true when its
        // link is clicked.
        let local_links = collect_local_links(&snippet.content);
        for url in &local_links {
            view.markdown_cache.add_link_hook(url.clone());
        }

        let mut open_snippet = None;
        ui.scope(|ui| {
            // Slightly larger body text for readability; headings derive from it.
            ui.style_mut().text_styles.insert(
                egui::TextStyle::Body,
                egui::FontId::proportional(16.0),
            );
            let output = egui_commonmark::CommonMarkViewer::new()
                .show(ui, &mut view.markdown_cache, &snippet.content);
            if let Some(url) = local_links
                .iter()
                .find(|url| view.markdown_cache.get_link_hook(url) == Some(true))
            {
                open_snippet = parse_snippet_link(url);
            }
            let _ = output;
        });

        // Right-click anywhere in the visible preview pane (content or the
        // empty area below it) opens the view-mode menu. The CommonMark
        // viewer's aggregate response only senses hover, so a manual check over
        // the whole pane is used instead of `response.context_menu`.
        let secondary_clicked = ui.input(|input| input.pointer.secondary_clicked());
        if secondary_clicked
            && let Some(pos) = ui.input(|input| input.pointer.interact_pos())
            && pane_rect.contains(pos)
        {
            view.mode_menu = Some(pos);
        }

        open_snippet
    }

    /// Renders the right-click view-mode menu as a manual `egui::Area` popup
    /// (a real context menu needs a click-sensing response, which the markdown
    /// viewer and the empty pane areas do not provide).
    fn render_view_mode_menu(ctx: &egui::Context, view: &mut View) {
        let Some(anchor) = view.mode_menu else {
            return;
        };
        let mut selected = None;
        let response = egui::Area::new(egui::Id::new(("snippet-view-menu", view.id)))
            .order(egui::Order::Foreground)
            .fixed_pos(anchor)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    selected = Self::mode_menu_items(ui, view);
                });
            });
        if let Some(mode) = selected {
            Self::apply_view_mode(view, mode);
            view.mode_menu = None;
            return;
        }
        let menu_rect = response.response.rect;
        let click_away = ctx.input(|i| i.pointer.any_pressed())
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|pos| !menu_rect.contains(pos));
        if click_away || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            view.mode_menu = None;
        }
    }

    pub(super) fn render_snippet_viewport(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
    ) -> ViewAction {
        ui.show_viewport_immediate(
            egui::ViewportId::from_hash_of(("snippet-view", view.id)),
            egui::ViewportBuilder::default()
                .with_title(if view.mode.is_editing() {
                    format!("[Edit] {} - FloatDea", snippet.title)
                } else {
                    format!("{} - FloatDea", snippet.title)
                })
                .with_inner_size([480.0, 320.0]),
            |child_ui, _| {
                let mut action = ViewAction::None;
                egui::CentralPanel::default()
                    .frame(
                        egui::Frame::new()
                            .inner_margin(egui::Margin::same(16))
                            .fill(child_ui.visuals().panel_fill),
                    )
                    .show(child_ui, |ui| {
                        egui::Frame::new()
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| match view.mode {
                                ViewMode::Preview => {
                                    let pane_rect = ui.available_rect_before_wrap();
                                    egui::ScrollArea::vertical()
                                        .id_salt(("preview-scroll", view.id))
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            if let Some(id) = Self::render_markdown_preview(
                                                ui, view, snippet, pane_rect,
                                            ) {
                                                action = ViewAction::OpenSnippet(id);
                                            }
                                        });
                                }
                                ViewMode::Source => {
                                    let pane_rect = ui.available_rect_before_wrap();
                                    egui::ScrollArea::vertical()
                                        .id_salt(("source-scroll", view.id))
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            Self::render_snippet_content(
                                                ui, view, snippet, store, pane_rect,
                                            );
                                        });
                                }
                            });
                    });
                // The right-click view-mode menu floats above the content.
                Self::render_view_mode_menu(child_ui.ctx(), view);
                if child_ui.input(|input| input.viewport().close_requested()) {
                    action = ViewAction::Close;
                }
                action
            },
        )
    }
}

/// Collects the destinations of inline markdown links (`[text](dest)`) that
/// point at local snippet files (`*.md`). Reference definitions
/// (`[id]: url`) are not handled yet.
fn collect_local_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b']' && bytes.get(i + 1) == Some(&b'(') {
            let mut end = i + 2;
            let mut depth = 1usize;
            while end < bytes.len() && depth > 0 {
                match bytes[end] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                end += 1;
            }
            if depth == 0 {
                let destination = &text[i + 2..end - 1];
                if destination.ends_with(".md") {
                    links.push(destination.to_owned());
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    links
}

/// Parses a `{title}--{id}.md` link destination into the referenced snippet
/// id. The title may itself contain `--`, so the id is the trailing segment.
fn parse_snippet_link(url: &str) -> Option<EntityId> {
    let file = url.strip_suffix(".md")?;
    let id = file.rsplit("--").next()?;
    let plausible = id.len() == 20 && id.bytes().all(|b| b.is_ascii_alphanumeric());
    plausible.then(|| EntityId::from_string(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_local_markdown_links() {
        let text = "see [a](hello--abc.md) and [b](https://example.com/x) and ![img](p.png)";
        assert_eq!(collect_local_links(text), vec!["hello--abc.md"]);
    }

    #[test]
    fn parses_snippet_link_id_from_filename() {
        let id = parse_snippet_link("my note--0123456789abcdef0123.md")
            .expect("link should parse");
        assert_eq!(id.as_str(), "0123456789abcdef0123");
        // Titles may contain `--`: the trailing segment wins.
        let id = parse_snippet_link("a--b--0123456789abcdef0123.md").unwrap();
        assert_eq!(id.as_str(), "0123456789abcdef0123");
        // Non-snippet links and malformed ids are rejected.
        assert!(parse_snippet_link("https://example.com/a.md").is_none());
        assert!(parse_snippet_link("note--short.md").is_none());
    }
}
