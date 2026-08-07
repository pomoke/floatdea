use egui::TextBuffer;

use super::*;

impl HomePage {
    /// Shared view-mode selection (`Source` / `Preview`). Returns the mode
    /// that was just selected, if any. Used by the editor's context menu and
    /// the preview's right-click popup.
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

    /// Renders the raw-markdown source editor (Source mode). `Esc` switches
    /// back to the preview. The right-click menu offers copy, "Insert Link…",
    /// and pasting a clipboard reference link at the editor cursor.
    fn render_snippet_content(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
        pane_rect: egui::Rect,
        snippet_index: &[(EntityId, String)],
        clipboard: &Option<ClipboardEntry>,
    ) {
        // Global per-view id: independent of the parent `Ui` (columns created
        // by `ui.columns` share one stable id, which would otherwise make the
        // editor state ambiguous in Split mode). Also preserves cursor/IME
        // state across mode switches.
        let text_edit_id = egui::Id::new(("snippet-content", view.id));
        let saved_selection = egui::TextEdit::load_state(ui.ctx(), text_edit_id)
            .and_then(|state| state.cursor.char_range());
        let secondary_pressed = ui.input(|input| input.pointer.secondary_pressed());

        // While the "Insert Link…" picker is open, Esc belongs to the picker
        // (it closes it), not to the editor.
        if view.link_picker.is_none() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
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
            // Copy the selection (or everything).
            if let Some(selection) = selection {
                let selected = snippet
                    .content
                    .char_range(selection.as_sorted_char_range())
                    .to_owned();
                if !selected.is_empty() && ui.button("Copy").clicked() {
                    ui.copy_text(selected);
                    ui.close();
                } else if selected.is_empty() && ui.button("Copy All").clicked() {
                    ui.copy_text(snippet.content.clone());
                    ui.close();
                }
            } else if ui.button("Copy All").clicked() {
                ui.copy_text(snippet.content.clone());
                ui.close();
            }
            ui.separator();

            // Insert a reference link at the editor cursor.
            let cursor = egui::TextEdit::load_state(ui.ctx(), text_edit_id)
                .and_then(|state| state.cursor.char_range())
                .map(|range| range.primary.index.0)
                .unwrap_or_else(|| snippet.content.chars().count());
            if ui.button("Insert Link…").clicked() {
                view.link_picker = Some(LinkPicker {
                    cursor,
                    filter: String::new(),
                    focus_requested: false,
                });
                ui.close();
            }
            // Paste a clipboard reference (Link/Move from a card's menu).
            if let Some(entry) = clipboard.as_ref()
                && let ReferenceTarget::Snippet(target_id) = &entry.target
                && let Some((_, title)) = snippet_index.iter().find(|(id, _)| id == target_id)
            {
                let button = format!("Paste Link: {title}");
                if ui.button(button).clicked() {
                    insert_markdown_link(&mut snippet.content, cursor, title, target_id);
                    let _ = store.save(snippet);
                    ui.close();
                }
            }
            ui.separator();
            if let Some(mode) = Self::mode_menu_items(ui, view) {
                Self::apply_view_mode(view, mode);
                ui.close();
            }
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

    /// Renders the CommonMark preview. Local `.md` links are hooked so a click
    /// returns the target [`EntityId`] to open; links that point at a
    /// non-existent snippet are flagged with a warning banner. A right-click
    /// anywhere in the visible pane (content or the empty area below it) opens
    /// the view-mode menu ([`View::mode_menu`]).
    fn render_markdown_preview(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        pane_rect: egui::Rect,
        snippet_index: &[(EntityId, String)],
    ) -> Option<EntityId> {
        // Only internal references (no URL scheme) are hooked so a click can be
        // intercepted; scheme'd URLs remain normal hyperlinks.
        let internal_links: Vec<String> = collect_local_links(&snippet.content)
            .into_iter()
            .filter(|url| !url.contains("://") && !url.starts_with("data:"))
            .collect();
        for url in &internal_links {
            // `prepare_show` resets hook values at the start of the frame, then
            // `Link::end` marks a hook true when its link is clicked.
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
            // A clicked internal link resolves to an existing snippet id;
            // missing or malformed targets raise a transient error instead.
            if let Some(url) = internal_links
                .iter()
                .find(|url| view.markdown_cache.get_link_hook(url) == Some(true))
            {
                if is_broken_internal_link(url, snippet_index) {
                    view.link_error = Some((format!("{url}\ndoes not exist."), 180));
                } else if let Some(id) = parse_snippet_link(url) {
                    open_snippet = Some(id);
                }
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

    /// Renders the "Insert Link…" picker window: a filterable list of snippets.
    /// Selecting one inserts `[title]({id}.md)` at the captured editor cursor.
    fn render_link_picker(
        ctx: &egui::Context,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
        snippet_index: &[(EntityId, String)],
    ) {
        let Some(mut picker) = view.link_picker.take() else {
            return;
        };
        let mut selected: Option<EntityId> = None;
        let mut cancelled = false;
        let response = egui::Window::new("Insert Link")
            .id(egui::Id::new(("insert-link-picker", view.id)))
            .collapsible(false)
            .resizable(false)
            .default_width(260.0)
            .show(ctx, |ui| {
                let filter = ui.add(
                    egui::TextEdit::singleline(&mut picker.filter)
                        .id(egui::Id::new(("insert-link-filter", view.id)))
                        .hint_text("Filter…"),
                );
                // Focus the filter once when the picker opens so typing
                // narrows the list immediately (IME-safe: only once).
                if !picker.focus_requested {
                    filter.request_focus();
                    picker.focus_requested = true;
                }
                ui.separator();
                // The list scrolls and fits the viewport instead of growing
                // the window without bounds.
                let list_height =
                    (ui.ctx().viewport_rect().height() - 130.0).clamp(80.0, 300.0);
                egui::ScrollArea::vertical()
                    .id_salt(("insert-link-list", view.id))
                    .max_height(list_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let filter = picker.filter.as_str();
                        let mut shown = 0;
                        for (id, title) in snippet_index {
                            if !filter.is_empty() && !title.contains(filter) {
                                continue;
                            }
                            shown += 1;
                            if ui.selectable_label(false, title).clicked() {
                                selected = Some(id.clone());
                            }
                        }
                        if shown == 0 {
                            ui.label("(no matches)");
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
        // Esc dismisses the picker without inserting.
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.input_mut(|i| i.consume_key(i.modifiers, egui::Key::Escape));
            return;
        }
        if response.is_none() || cancelled {
            // Closed via the close button or "Cancel".
            return;
        }
        if let Some(target_id) = selected {
            let title = snippet_index
                .iter()
                .find(|(id, _)| id == &target_id)
                .map(|(_, title)| title.clone())
                .unwrap_or_default();
            insert_markdown_link(&mut snippet.content, picker.cursor, &title, &target_id);
            let _ = store.save(snippet);
            return;
        }
        // Not selected yet: keep the picker open.
        view.link_picker = Some(picker);
    }

    pub(super) fn render_snippet_viewport(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
        snippet_index: &[(EntityId, String)],
        clipboard: &Option<ClipboardEntry>,
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
                let panel = egui::CentralPanel::default()
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
                                                ui,
                                                view,
                                                snippet,
                                                pane_rect,
                                                snippet_index,
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
                                                ui,
                                                view,
                                                snippet,
                                                store,
                                                pane_rect,
                                                snippet_index,
                                                clipboard,
                                            );
                                        });
                                }
                            });
                    });

                // Cross-window drag & drop: a card dragged from a canvas and
                // released over this note inserts a reference link at the
                // editor cursor (or appends when there is no editor cursor).
                let ctx = child_ui.ctx();
                let drop_payload = egui::DragAndDrop::payload::<DragPayload>(ctx)
                    .map(|payload| (*payload).clone());
                if let Some(DragPayload::Reference {
                    target: ReferenceTarget::Snippet(target_id),
                    ..
                }) = &drop_payload
                    && ctx.input(|input| input.pointer.primary_released())
                    && ctx
                        .input(|input| input.pointer.interact_pos())
                        .is_some_and(|pos| panel.response.rect.contains(pos))
                {
                    if let Some((_, title)) = snippet_index.iter().find(|(id, _)| id == target_id)
                    {
                        let cursor = egui::TextEdit::load_state(
                            ctx,
                            egui::Id::new(("snippet-content", view.id)),
                        )
                        .and_then(|state| state.cursor.char_range())
                        .map(|range| range.primary.index.0)
                        .unwrap_or_else(|| snippet.content.chars().count());
                        insert_markdown_link(&mut snippet.content, cursor, title, target_id);
                        let _ = store.save(snippet);
                        ctx.request_repaint();
                    }
                    egui::DragAndDrop::clear_payload(ctx);
                }

                // Broken-link click errors float as a toast (like the
                // clipboard status), auto-dismissed after a short while.
                let mut dismiss_error = false;
                if let Some((message, frames)) = &mut view.link_error {
                    *frames = frames.saturating_sub(1);
                    if *frames == 0 {
                        dismiss_error = true;
                    } else {
                        egui::Window::new("link-error")
                            .id(egui::Id::new(("snippet-link-error", view.id)))
                            .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0))
                            .collapsible(false)
                            .resizable(false)
                            .title_bar(false)
                            .show(ctx, |ui| {
                                ui.colored_label(ui.visuals().error_fg_color, message.as_str());
                            });
                    }
                }
                if dismiss_error {
                    view.link_error = None;
                }

                // The right-click view-mode menu floats above the content.
                Self::render_view_mode_menu(ctx, view);
                // The "Insert Link…" picker window.
                Self::render_link_picker(ctx, view, snippet, store, snippet_index);
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

/// Parses a link destination (`{title}--{id}.md` or `{id}.md`) into the
/// referenced snippet id. The title may itself contain `--`, so the id is the
/// trailing segment.
fn parse_snippet_link(url: &str) -> Option<EntityId> {
    let file = url.strip_suffix(".md")?;
    let id = file.rsplit("--").next()?;
    let plausible = id.len() == 20 && id.bytes().all(|b| b.is_ascii_alphanumeric());
    plausible.then(|| EntityId::from_string(id))
}

/// Inserts `[title]({id}.md)` into `content` at `cursor` (a char index). The
/// link target carries only the stable id — never the title, which can change.
fn insert_markdown_link(content: &mut String, cursor: usize, title: &str, id: &EntityId) {
    let link = format!("[{title}]({}.md)", id.as_str());
    let index = cursor.min(content.chars().count());
    content.insert_text(&link, egui::text::CharIndex(index));
}

/// Whether a `.md` link destination is an internal reference (no URL scheme)
/// that does not resolve to an existing snippet — either the parsed id is
/// missing, or the target is not a valid snippet reference at all (e.g.
/// `foo.md`). Scheme'd URLs are external and left to the browser.
fn is_broken_internal_link(url: &str, snippet_index: &[(EntityId, String)]) -> bool {
    if url.contains("://") || url.starts_with("data:") {
        return false;
    }
    !parse_snippet_link(url)
        .is_some_and(|id| snippet_index.iter().any(|(existing, _)| existing == &id))
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
        // The id-only form (`{id}.md`, no title) parses too.
        let id = parse_snippet_link("0123456789abcdef0123.md").unwrap();
        assert_eq!(id.as_str(), "0123456789abcdef0123");
        // Non-snippet links and malformed ids are rejected.
        assert!(parse_snippet_link("https://example.com/a.md").is_none());
        assert!(parse_snippet_link("note--short.md").is_none());
    }

    #[test]
    fn flags_links_to_missing_snippets() {
        let index = vec![(EntityId::from_string("0123456789abcdef0123"), "Exists".to_owned())];
        let cases = [
            ("0123456789abcdef0123.md", false, "existing"),
            ("aaaaaaaaaaaaaaaaaaaa.md", true, "missing id"),
            ("foo.md", true, "malformed"),
            ("note--short.md", true, "old short id"),
            ("https://x.example/a.md", false, "external"),
        ];
        for (url, expected, label) in cases {
            assert_eq!(
                is_broken_internal_link(url, &index),
                expected,
                "{label}: {url}"
            );
        }
    }

    #[test]
    fn inserts_markdown_link_with_id_only_target() {
        let mut content = "hello world".to_owned();
        let id = EntityId::from_string("0123456789abcdef0123");
        insert_markdown_link(&mut content, 5, "My Note", &id);
        // Inserted at the cursor (char index 5); the target holds only the id.
        assert_eq!(content, "hello[My Note](0123456789abcdef0123.md) world");
        // Cursors beyond the end are clamped to the end.
        let mut content = String::new();
        insert_markdown_link(&mut content, 999, "Note", &id);
        assert_eq!(content, "[Note](0123456789abcdef0123.md)");
    }
}
