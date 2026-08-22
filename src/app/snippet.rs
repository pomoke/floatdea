use egui::TextBuffer;

use super::*;
use super::image::{ImageBytesCache, ImageBytesKey};

impl HomePage {
    /// Shared view-mode selection (`Source` / `Preview`). Returns the mode
    /// that was just selected, if any. Used by the editor's context menu and
    /// the preview's right-click popup.
    fn mode_menu_items(ui: &mut egui::Ui, view: &View) -> Option<ViewMode> {
        // Plain labels: decorative glyphs (e.g. "✎") are not covered by the
        // installed fonts and render as tofu.
        if view.read_only {
            // Linked Source preview: the editor is intentionally unreachable.
            let _ = ui.selectable_label(true, "Preview (read-only)");
            return None;
        }
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

    /// Window title for a snippet view: read-only Linked Source previews carry
    /// a persistent "Linked Source · Read-only" prefix; editing windows show
    /// "[Edit]"; everything else shows the plain title.
    fn view_title(view: &View, snippet_title: &str) -> String {
        if view.read_only {
            format!("Linked Source · Read-only — {snippet_title} - FloatDea")
        } else if view.mode.is_editing() {
            format!("[Edit] {snippet_title} - FloatDea")
        } else {
            format!("{snippet_title} - FloatDea")
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
    /// "Insert Image…", "Insert Existing Image…", and pasting a clipboard
    /// reference link at the editor cursor.
    fn render_snippet_content(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
        pane_rect: egui::Rect,
        snippet_index: &[(EntityId, String, String)],
        clipboard: &Option<ClipboardEntry>,
    ) -> ViewAction {
        let mut action = ViewAction::None;
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
                    embed: false,
                });
                ui.close();
            }
            if ui.button("Insert Image…").clicked() {
                action = ViewAction::ChooseSnippetImage {
                    view_id: view.id,
                    cursor,
                };
                ui.close();
            }
            if ui.button("Insert Existing Image…").clicked() {
                view.image_picker = Some(ImagePicker {
                    cursor,
                    filter: String::new(),
                    focus_requested: false,
                });
                ui.close();
            }
            // Paste a clipboard reference (Link/Move from a card's menu).
            if let Some(entry) = clipboard.as_ref()
                && let ReferenceTarget::Snippet(target_id) = &entry.target
                && let Some((_, title, _)) = snippet_index.iter().find(|(id, _, _)| id == target_id)
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
        action
    }

    /// Renders the CommonMark preview. `![alt]({id}.md)` embeds render the
    /// target's whole document inline (non-recursively); local `.md` links are
    /// hooked so a click opens the target, with a transient error for broken
    /// ones. A right-click anywhere in the visible pane (content or the empty
    /// area below it) opens the view-mode menu ([`View::mode_menu`]). Local
    /// image embeds (`![alt](attachments/{id}.{ext})`) render inline; clicking
    /// one opens the in-app image viewer.
    #[allow(clippy::too_many_arguments)]
    fn render_markdown_preview(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        pane_rect: egui::Rect,
        snippet_index: &[(EntityId, String, String)],
        math_renderer: &MathRenderer,
        settings: &Settings,
        images: &BTreeMap<AttachmentId, ImageAttachment>,
        attachment_store: &AttachmentStore,
        image_cache: &mut ImageBytesCache,
    ) -> ViewAction {
        // Internal references (no URL scheme) are hooked so a click can be
        // intercepted; external URLs (with a scheme) are also hooked so we can
        // show a confirmation dialog before opening them in the browser.
        let internal_links: Vec<String> = collect_local_links(&snippet.content)
            .into_iter()
            .filter(|url| !url.contains("://") && !url.starts_with("data:"))
            .collect();
        let external_links: Vec<String> = collect_external_links(&snippet.content);
        for url in internal_links.iter().chain(external_links.iter()) {
            // `prepare_show` resets hook values at the start of the frame, then
            // `Link::end` marks a hook true when its link is clicked.
            view.markdown_cache.add_link_hook(url.clone());
        }

        let mut action = ViewAction::None;
        let segments = split_embeds(&snippet.content);
        let math_cap_scale = settings.math_cap_scale;
        let callback_renderer = math_renderer.clone();
        let render_math = move |ui: &mut egui::Ui, source: &str, inline: bool| {
            callback_renderer.show(ui, source, inline, math_cap_scale);
        };
        ui.scope(|ui| {
            // Body text size comes from settings; headings derive from it.
            ui.style_mut().text_styles.insert(
                egui::TextStyle::Body,
                egui::FontId::proportional(settings.preview_font_size),
            );
            for segment in &segments {
                match segment {
                    PreviewSegment::Text(text) => {
                        let _ = egui_commonmark::CommonMarkViewer::new()
                            .render_math_fn(Some(&render_math))
                            .show(ui, &mut view.markdown_cache, text);
                    }
                    PreviewSegment::Embed { dest, .. } => {
                        if let Some(embed_action) = Self::render_embed_card(
                            ui,
                            view,
                            dest,
                            snippet_index,
                            math_renderer,
                            settings,
                        ) {
                            match embed_action {
                                EmbedLinkAction::OpenSnippet(id) => {
                                    action = ViewAction::OpenSnippet(id)
                                }
                                EmbedLinkAction::OpenExternal(url) => {
                                    view.pending_external = Some(url);
                                }
                            }
                        }
                    }
                    PreviewSegment::Image { alt, dest } => {
                        if let Some(id) = render_preview_image(
                            ui,
                            dest,
                            alt,
                            images,
                            attachment_store,
                            image_cache,
                        ) {
                            action = ViewAction::OpenImage(id);
                        }
                    }
                }
            }
            // A clicked internal link resolves to an existing snippet id;
            // missing or malformed targets raise a transient error instead.
            if let Some(url) = internal_links
                .iter()
                .find(|url| view.markdown_cache.get_link_hook(url) == Some(true))
            {
                if is_broken_internal_link(url, snippet_index) {
                    view.link_error = Some((format!("{url}\ndoes not exist."), 180));
                } else if let Some(id) = parse_snippet_link(url) {
                    action = ViewAction::OpenSnippet(id);
                }
            }
            // A clicked external URL is queued for confirmation: FloatDea asks
            // before handing it to the browser.
            if let Some(url) = external_links
                .iter()
                .find(|url| view.markdown_cache.get_link_hook(url) == Some(true))
            {
                view.pending_external = Some(url.clone());
            }
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

        action
    }

    /// Renders one inline embed as a framed card: the target's **whole
    /// document**, rendered once and non-recursively (nested `![` degrades to
    /// plain links). No header is shown. Returns the action for a link clicked
    /// inside the embedded document (open a snippet, or an external URL).
    fn render_embed_card(
        ui: &mut egui::Ui,
        view: &mut View,
        dest: &str,
        snippet_index: &[(EntityId, String, String)],
        math_renderer: &MathRenderer,
        settings: &Settings,
    ) -> Option<EmbedLinkAction> {
        let Some(id) = parse_snippet_link(dest) else {
            ui.colored_label(ui.visuals().warn_fg_color, format!("invalid embed: {dest}"));
            return None;
        };
        let Some((_, _, content)) = snippet_index
            .iter()
            .find(|(existing, _, _)| existing == &id)
        else {
            ui.colored_label(ui.visuals().warn_fg_color, format!("missing page: {dest}"));
            return None;
        };
        // The embedded document is rendered non-recursively (nested `![]`
        // degrade to plain links), so collect its links from the neutralized
        // text and hook them so clicks resolve here.
        let neutralized = neutralize_embeds(content);
        let internal_links: Vec<String> = collect_local_links(&neutralized)
            .into_iter()
            .filter(|url| !url.contains("://") && !url.starts_with("data:"))
            .collect();
        let external_links: Vec<String> = collect_external_links(&neutralized);
        for url in internal_links.iter().chain(external_links.iter()) {
            view.markdown_cache.add_link_hook(url.clone());
        }
        let mut clicked: Option<EmbedLinkAction> = None;
        egui::Frame::new()
            .inner_margin(egui::Margin::same(8))
            .stroke(egui::Stroke::new(
                1.0,
                ui.visuals().widgets.noninteractive.bg_stroke.color,
            ))
            .corner_radius(egui::CornerRadius::same(4))
            .show(ui, |ui| {
                // Non-recursive: embed markers inside the target become links.
                let math_cap_scale = settings.math_cap_scale;
                let math_renderer = math_renderer.clone();
                let render_math = move |ui: &mut egui::Ui, source: &str, inline: bool| {
                    math_renderer.show(ui, source, inline, math_cap_scale);
                };
                let _ = egui_commonmark::CommonMarkViewer::new()
                    .render_math_fn(Some(&render_math))
                    .show(ui, &mut view.markdown_cache, &neutralized);
            });
        // A clicked internal link inside the embed opens the referenced snippet.
        if let Some(url) = internal_links
            .iter()
            .find(|url| view.markdown_cache.get_link_hook(url) == Some(true))
            && let Some(target_id) = parse_snippet_link(url)
        {
            clicked = Some(EmbedLinkAction::OpenSnippet(target_id));
        }
        // A clicked external URL inside the embed is queued for confirmation.
        if let Some(url) = external_links
            .iter()
            .find(|url| view.markdown_cache.get_link_hook(url) == Some(true))
        {
            clicked = Some(EmbedLinkAction::OpenExternal(url.clone()));
        }
        ui.add_space(8.0);
        clicked
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

    /// Renders a confirmation dialog before opening an external URL in the
    /// browser. Called when the user clicks a scheme'd link in a preview.
    fn render_external_link_confirm(ctx: &egui::Context, view: &mut View) {
        let Some(url) = view.pending_external.clone() else {
            return;
        };
        let mut open = true;
        let mut open_confirm = false;
        let mut dismiss = false;
        egui::Window::new("Open external link?")
            .id(egui::Id::new(("external-link-confirm", view.id)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.label("This link will open in your browser:");
                ui.add_space(4.0);
                ui.label(egui::RichText::new(&url).weak());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Open").clicked() {
                        open_confirm = true;
                    }
                    if ui.button("Cancel").clicked() {
                        dismiss = true;
                    }
                });
            });
        if open_confirm {
            view.pending_external = None;
            let _ = open_path_externally(&url);
        } else if dismiss || !open {
            // "Cancel" button or the window's close button.
            view.pending_external = None;
        }
    }

    /// Renders the "Insert Link…" picker window: a filterable list of snippets.
    /// Selecting one inserts `[title]({id}.md)` at the captured editor cursor.
    fn render_link_picker(
        ctx: &egui::Context,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
        snippet_index: &[(EntityId, String, String)],
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
                ui.checkbox(&mut picker.embed, "Embed whole document");
                ui.separator();
                // The list scrolls and fits the viewport instead of growing
                // the window without bounds.
                let list_height = (ui.ctx().viewport_rect().height() - 130.0).clamp(80.0, 300.0);
                egui::ScrollArea::vertical()
                    .id_salt(("insert-link-list", view.id))
                    .max_height(list_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let filter = picker.filter.as_str();
                        let mut shown = 0;
                        for (id, title, _) in snippet_index {
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
                .find(|(id, _, _)| id == &target_id)
                .map(|(_, title, _)| title.clone())
                .unwrap_or_default();
            if picker.embed {
                insert_embed_markdown(&mut snippet.content, picker.cursor, &title, &target_id);
            } else {
                insert_markdown_link(&mut snippet.content, picker.cursor, &title, &target_id);
            }
            let _ = store.save(snippet);
            return;
        }
        // Not selected yet: keep the picker open.
        view.link_picker = Some(picker);
    }

    /// Renders the "Insert Existing Image…" picker window: a filterable list of
    /// managed workspace images. Selecting one inserts
    /// `![title](attachments/{id}.{ext})` at the captured editor cursor
    /// (reusing the existing attachment, never copying the file again).
    fn render_image_picker(
        ctx: &egui::Context,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
        images: &BTreeMap<AttachmentId, ImageAttachment>,
    ) {
        let Some(mut picker) = view.image_picker.take() else {
            return;
        };
        let mut selected: Option<AttachmentId> = None;
        let mut cancelled = false;
        let response = egui::Window::new("Insert Existing Image")
            .id(egui::Id::new(("insert-image-picker", view.id)))
            .collapsible(false)
            .resizable(false)
            .default_width(280.0)
            .show(ctx, |ui| {
                let filter = ui.add(
                    egui::TextEdit::singleline(&mut picker.filter)
                        .id(egui::Id::new(("insert-image-filter", view.id)))
                        .hint_text("Filter…"),
                );
                if !picker.focus_requested {
                    filter.request_focus();
                    picker.focus_requested = true;
                }
                ui.separator();
                let list_height = (ui.ctx().viewport_rect().height() - 130.0).clamp(80.0, 300.0);
                egui::ScrollArea::vertical()
                    .id_salt(("insert-image-list", view.id))
                    .max_height(list_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        let filter = picker.filter.as_str();
                        let mut shown = 0;
                        for image in images.values() {
                            if !filter.is_empty() && !image.title.contains(filter) {
                                continue;
                            }
                            shown += 1;
                            if ui.selectable_label(false, &image.title).clicked() {
                                selected = Some(image.id.clone());
                            }
                        }
                        if shown == 0 {
                            ui.label(if images.is_empty() {
                                "(no images yet — use Insert Image… to add one)"
                            } else {
                                "(no matches)"
                            });
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                });
            });
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.input_mut(|i| i.consume_key(i.modifiers, egui::Key::Escape));
            return;
        }
        if response.is_none() || cancelled {
            return;
        }
        if let Some(id) = selected {
            if let Some(image) = images.get(&id) {
                let alt = markdown_alt_text(&image.title);
                let markdown = format!("![{alt}]({})", image.relative_path);
                let index = picker.cursor.min(snippet.content.chars().count());
                snippet
                    .content
                    .insert_text(&markdown, egui::text::CharIndex(index));
                let _ = store.save(snippet);
            }
            return;
        }
        // Not selected yet: keep the picker open.
        view.image_picker = Some(picker);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_snippet_viewport(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
        snippet_index: &[(EntityId, String, String)],
        clipboard: &Option<ClipboardEntry>,
        math_renderer: &MathRenderer,
        settings: &Settings,
        images: &BTreeMap<AttachmentId, ImageAttachment>,
        attachment_store: &AttachmentStore,
        image_cache: &mut ImageBytesCache,
    ) -> ViewAction {
        ui.show_viewport_immediate(
            egui::ViewportId::from_hash_of(("snippet-view", view.id)),
            egui::ViewportBuilder::default()
                .with_title(Self::view_title(view, &snippet.title))
                .with_inner_size([480.0, 320.0]),
            |child_ui, _| {
                let mut action = Self::render_snippet_panel(
                    child_ui,
                    view,
                    snippet,
                    store,
                    snippet_index,
                    clipboard,
                    math_renderer,
                    settings,
                    images,
                    attachment_store,
                    image_cache,
                );
                if child_ui.input(|input| input.viewport().close_requested()) {
                    action = ViewAction::Close;
                }
                action
            },
        )
    }

    /// Renders a snippet as a floating window inside the main window (full-window
    /// mode). Same content as [`Self::render_snippet_viewport`].
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_snippet_window(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
        snippet_index: &[(EntityId, String, String)],
        clipboard: &Option<ClipboardEntry>,
        math_renderer: &MathRenderer,
        settings: &Settings,
        images: &BTreeMap<AttachmentId, ImageAttachment>,
        attachment_store: &AttachmentStore,
        image_cache: &mut ImageBytesCache,
    ) -> ViewAction {
        let mut open = true;
        let mut action = ViewAction::None;
        egui::Window::new(Self::view_title(view, &snippet.title))
        .id(egui::Id::new(("snippet-window", view.id)))
        .open(&mut open)
        .default_size([480.0, 320.0])
        .show(ui.ctx(), |ui| {
            action = Self::render_snippet_panel(
                ui,
                view,
                snippet,
                store,
                snippet_index,
                clipboard,
                math_renderer,
                settings,
                images,
                attachment_store,
                image_cache,
            );
        });
        if !open {
            action = ViewAction::Close;
        }
        action
    }

    /// The body shared by the native snippet viewport and the floating snippet
    /// window: the content panel, cross-window drop target, error toast,
    /// view-mode menu, and "Insert Link…"/"Insert Existing Image…" pickers.
    #[allow(clippy::too_many_arguments)]
    fn render_snippet_panel(
        ui: &mut egui::Ui,
        view: &mut View,
        snippet: &mut Snippet,
        store: &SnippetStore,
        snippet_index: &[(EntityId, String, String)],
        clipboard: &Option<ClipboardEntry>,
        math_renderer: &MathRenderer,
        settings: &Settings,
        images: &BTreeMap<AttachmentId, ImageAttachment>,
        attachment_store: &AttachmentStore,
        image_cache: &mut ImageBytesCache,
    ) -> ViewAction {
        let mut action = ViewAction::None;
        // Linked Source previews never enter the editor (even if a stale state
        // or menu action tried to switch modes).
        if view.read_only {
            view.mode = ViewMode::Preview;
        }
        let panel = egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(16))
                    .fill(ui.visuals().panel_fill),
            )
            .show(ui, |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| match view.mode {
                        ViewMode::Preview => {
                            let pane_rect = ui.available_rect_before_wrap();
                            egui::ScrollArea::vertical()
                                .id_salt(("preview-scroll", view.id))
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    action = Self::render_markdown_preview(
                                        ui,
                                        view,
                                        snippet,
                                        pane_rect,
                                        snippet_index,
                                        math_renderer,
                                        settings,
                                        images,
                                        attachment_store,
                                        image_cache,
                                    );
                                });
                        }
                        ViewMode::Source => {
                            let pane_rect = ui.available_rect_before_wrap();
                            egui::ScrollArea::vertical()
                                .id_salt(("source-scroll", view.id))
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    action = Self::render_snippet_content(
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
        let ctx = ui.ctx();
        let drop_payload =
            egui::DragAndDrop::payload::<DragPayload>(ctx).map(|payload| (*payload).clone());
        if let Some(DragPayload::Reference {
            target: ReferenceTarget::Snippet(target_id),
            ..
        }) = &drop_payload
            && ctx.input(|input| input.pointer.primary_released())
            && ctx
                .input(|input| input.pointer.interact_pos())
                .is_some_and(|pos| panel.response.rect.contains(pos))
        {
            if let Some((_, title, _)) = snippet_index.iter().find(|(id, _, _)| id == target_id) {
                let cursor =
                    egui::TextEdit::load_state(ctx, egui::Id::new(("snippet-content", view.id)))
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
        // Confirmation before opening an external URL clicked in a preview.
        Self::render_external_link_confirm(ctx, view);
        // The "Insert Link…" picker window.
        Self::render_link_picker(ctx, view, snippet, store, snippet_index);
        // The "Insert Existing Image…" picker window.
        Self::render_image_picker(ctx, view, snippet, store, images);
        action
    }
}

/// One piece of a preview: plain markdown text, a `.md` inline embed that
/// renders the target's whole document inline, or a managed-image embed.
#[derive(Debug, PartialEq)]
pub(super) enum PreviewSegment {
    Text(String),
    Embed { alt: String, dest: String },
    Image { alt: String, dest: String },
}

/// A link clicked inside an embedded document.
#[derive(Debug, PartialEq)]
enum EmbedLinkAction {
    /// Open the referenced snippet.
    OpenSnippet(EntityId),
    /// Confirm and open an external URL in the browser.
    OpenExternal(String),
}

/// Splits `content` at inline embeds using the unified local-target parser:
/// `![alt](dest)` where `dest` is a local `*.md` reference becomes an
/// [`PreviewSegment::Embed`]; `attachments/{id}.{ext}` becomes an
/// [`PreviewSegment::Image`]. Plain links and external URLs stay in the text
/// segment (handled by CommonMark). Fenced code blocks, inline code and escaped
/// characters are skipped by the parser.
pub(super) fn split_embeds(content: &str) -> Vec<PreviewSegment> {
    let targets = parse_local_markdown_targets(content);
    let mut segments = Vec::new();
    let mut text_start = 0;
    for parsed in &targets {
        if !parsed.is_image {
            continue;
        }
        let segment = match &parsed.target {
            LocalMarkdownTarget::Snippet(_) => Some(PreviewSegment::Embed {
                alt: parsed.label.clone(),
                dest: parsed.dest.clone(),
            }),
            LocalMarkdownTarget::Image(_) => Some(PreviewSegment::Image {
                alt: parsed.label.clone(),
                dest: parsed.dest.clone(),
            }),
            LocalMarkdownTarget::UnresolvedLocal(_) => None,
        };
        let Some(segment) = segment else {
            continue;
        };
        if text_start < parsed.start {
            segments.push(PreviewSegment::Text(content[text_start..parsed.start].to_owned()));
        }
        segments.push(segment);
        text_start = parsed.end;
    }
    if text_start < content.len() {
        segments.push(PreviewSegment::Text(content[text_start..].to_owned()));
    }
    segments
}

/// Renders one managed-image embed (`![alt](attachments/{id}.{ext})`) with its
/// natural aspect ratio, capped at the available width. Missing or corrupt
/// images render a placeholder with the alt text. Returns the `AttachmentId`
/// when the image was clicked (opens the in-app viewer).
#[allow(clippy::too_many_arguments)]
fn render_preview_image(
    ui: &mut egui::Ui,
    dest: &str,
    alt: &str,
    images: &BTreeMap<AttachmentId, ImageAttachment>,
    attachment_store: &AttachmentStore,
    image_cache: &mut ImageBytesCache,
) -> Option<AttachmentId> {
    let Some(id) = parse_attachment_link(dest) else {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("invalid image embed: {dest}"),
        );
        return None;
    };
    let Some(image) = images.get(&id).cloned() else {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!("missing image: {dest}"),
        );
        return None;
    };

    // Size: max width = available text width, height by pixel ratio.
    let avail = ui.available_width();
    let [w, h] = image.pixel_size;
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let display = if w > avail {
        let scale = avail / w;
        egui::vec2(avail, (h * scale).max(1.0))
    } else {
        egui::vec2(w, h)
    };

    let key = ImageBytesKey::Attachment(image.id.clone());
    let bytes = if let Some(bytes) = image_cache.get(&key) {
        bytes
    } else {
        let Ok(raw) = image::load_attachment_bytes(attachment_store, &image) else {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("could not read image: {alt}"),
            );
            return None;
        };
        let bytes: egui::load::Bytes = raw.into();
        image_cache.insert(key, bytes.clone());
        bytes
    };
    let source = image::attachment_source(&image, bytes);
    let (rect, response) = ui.allocate_exact_size(display, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        egui::Image::new(source)
            .texture_options(egui::TextureOptions::LINEAR)
            .fit_to_exact_size(display)
            .paint_at(ui, rect);
    }
    if response.clicked() {
        return Some(image.id);
    }
    None
}

/// Replaces inline-embed markers with plain links (`![` → `[`), used when
/// rendering an embedded document non-recursively.
fn neutralize_embeds(text: &str) -> String {
    text.replace("![", "[")
}

/// Collects the destinations of inline markdown links (`[text](dest)`) that
/// point at local snippet files (`*.md`). Embed markers (`![alt](dest)`) are
/// excluded — they are handled by [`split_embeds`]. Reference definitions
/// (`[id]: url`) are not handled yet.
fn collect_local_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b']' && bytes.get(i + 1) == Some(&b'(') {
            // `![alt](dest)` is an embed, not a link.
            let mut open = i;
            while open > 0 && bytes[open] != b'[' {
                open -= 1;
            }
            let is_embed = open > 0 && bytes[open - 1] == b'!';
            if !is_embed {
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
                continue;
            }
        }
        i += 1;
    }
    links
}

/// Collects the destinations of inline markdown links that point outside the
/// workspace: any destination carrying a URL scheme (`://`, e.g. `https://…`)
/// or a `data:` URI. Embed markers (`![alt](dest)`) are excluded. External
/// links are hooked and confirmed before being opened in the browser.
pub(crate) fn collect_external_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b']' && bytes.get(i + 1) == Some(&b'(') {
            // `![alt](dest)` is an embed, not a link.
            let mut open = i;
            while open > 0 && bytes[open] != b'[' {
                open -= 1;
            }
            let is_embed = open > 0 && bytes[open - 1] == b'!';
            if !is_embed {
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
                    if destination.contains("://") || destination.starts_with("data:") {
                        links.push(destination.to_owned());
                    }
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    // Also collect angle-bracket autolinks (`<https://…>`), which pulldown
    // parses as links with the same destination. Only URL autolinks carry a
    // scheme; bare `<…>` (e.g. an HTML tag) is ignored.
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut end = i + 1;
            while end < bytes.len() && bytes[end] != b'>' {
                end += 1;
            }
            if end < bytes.len() {
                let destination = &text[i + 1..end];
                if destination.contains("://") || destination.starts_with("data:") {
                    links.push(destination.to_owned());
                }
                i = end + 1;
                continue;
            }
        }
        i += 1;
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

/// Inserts an inline embed `![title]({id}.md)` (renders the target's whole
/// document) into `content` at `cursor`.
fn insert_embed_markdown(content: &mut String, cursor: usize, title: &str, id: &EntityId) {
    let link = format!("![{title}]({}.md)", id.as_str());
    let index = cursor.min(content.chars().count());
    content.insert_text(&link, egui::text::CharIndex(index));
}

/// Whether a `.md` link destination is an internal reference (no URL scheme)
/// that does not resolve to an existing snippet — either the parsed id is
/// missing, or the target is not a valid snippet reference at all (e.g.
/// `foo.md`). Scheme'd URLs are external and left to the browser.
fn is_broken_internal_link(url: &str, snippet_index: &[(EntityId, String, String)]) -> bool {
    if url.contains("://") || url.starts_with("data:") {
        return false;
    }
    !parse_snippet_link(url)
        .is_some_and(|id| snippet_index.iter().any(|(existing, _, _)| existing == &id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_local_markdown_links() {
        let text = "see [a](hello--abc.md) and [b](https://example.com/x) \
                    and ![img](p.png) and ![emb](0123456789abcdef0123.md)";
        // Links only: embeds (`![...]`) are excluded.
        assert_eq!(collect_local_links(text), vec!["hello--abc.md"]);
    }

    #[test]
    fn collects_external_links() {
        let text = "see [site](https://example.com/x) and [docs](https://docs.rs/egui) \
                    and [data](data:image/png;base64,abc) and [local](0123456789abcdef0123.md) \
                    and ![img](p.png) and ![emb](https://x.example/a.md) \
                    and autolink <https://autolink.example.org> and <span>tag</span>";
        // Scheme'd link destinations only; embeds (`![...]`) excluded; local
        // `.md` references excluded; angle-bracket autolinks included; bare
        // `<span>` (no scheme) excluded.
        assert_eq!(
            collect_external_links(text),
            vec![
                "https://example.com/x".to_owned(),
                "https://docs.rs/egui".to_owned(),
                "data:image/png;base64,abc".to_owned(),
                "https://autolink.example.org".to_owned(),
            ]
        );
    }

    #[test]
    fn splits_embeds_and_text() {
        let content = "intro\n\n![Title](0123456789abcdef0123.md)\n\noutro";
        assert_eq!(
            split_embeds(content),
            vec![
                PreviewSegment::Text("intro\n\n".to_owned()),
                PreviewSegment::Embed {
                    alt: "Title".to_owned(),
                    dest: "0123456789abcdef0123.md".to_owned(),
                },
                PreviewSegment::Text("\n\noutro".to_owned()),
            ]
        );
        // No embeds → a single text segment.
        assert_eq!(
            split_embeds("plain text"),
            vec![PreviewSegment::Text("plain text".to_owned())]
        );
        // Images and external URLs share `![...]` syntax but are NOT document
        // embeds: they stay in the text segment (future real image embeds).
        assert_eq!(
            split_embeds("a ![img](p.png) ![ext](https://x.example/a.md) b"),
            vec![PreviewSegment::Text(
                "a ![img](p.png) ![ext](https://x.example/a.md) b".to_owned()
            )]
        );
    }

    #[test]
    fn neutralizes_embeds_to_links() {
        assert_eq!(
            neutralize_embeds("![a](x.md) and ![b](y.md)"),
            "[a](x.md) and [b](y.md)"
        );
    }

    #[test]
    fn parses_snippet_link_id_from_filename() {
        let id = parse_snippet_link("my note--0123456789abcdef0123.md").expect("link should parse");
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
        let index = vec![(
            EntityId::from_string("0123456789abcdef0123"),
            "Exists".to_owned(),
            String::new(),
        )];
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
