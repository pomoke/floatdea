use super::*;

pub(super) struct CanvasData<'a> {
    snippets: &'a mut BTreeMap<EntityId, Snippet>,
    workspace: &'a mut Workspace,
    workspace_store: &'a WorkspaceStore,
    snippet_store: &'a SnippetStore,
    clipboard: &'a mut Option<ClipboardEntry>,
}

impl<'a> CanvasData<'a> {
    pub(super) fn new(
        snippets: &'a mut BTreeMap<EntityId, Snippet>,
        workspace: &'a mut Workspace,
        workspace_store: &'a WorkspaceStore,
        snippet_store: &'a SnippetStore,
        clipboard: &'a mut Option<ClipboardEntry>,
    ) -> Self {
        Self {
            snippets,
            workspace,
            workspace_store,
            snippet_store,
            clipboard,
        }
    }
}

/// Outcome of rendering the rename dialog for a specific viewport.
enum RenameDialogResult {
    /// No dialog is pending, or it belongs to another viewport.
    None,
    /// The dialog is open and awaiting input.
    Open,
    /// The user confirmed the new title.
    Confirmed(RenameTarget),
    /// The user cancelled.
    Cancelled,
}

impl ContainerCanvas {
    fn default_position(&self, data: &CanvasData<'_>) -> [f32; 2] {
        default_position_for(
            &self.items,
            data.snippets,
            data.workspace,
            &approx_text_rects(&self.texts),
        )
    }

}

impl HomePage {
    pub(super) fn render_home_panel(&mut self, ui: &mut egui::Ui) -> Vec<CanvasCommand> {
        let mut data = CanvasData::new(
            &mut self.all_snippets,
            &mut self.workspace,
            &self.workspace_store,
            &self.store,
            &mut self.clipboard,
        );
        Self::render_canvas_panel(ui, &mut self.root, &mut data)
    }

    pub(super) fn render_delete_dialog(&mut self, ui: &mut egui::Ui) {
        let Some(pending) = self.pending_delete.clone() else {
            return;
        };
        let (title, kind) = match &pending.target {
            ReferenceTarget::Snippet(id) => match self.all_snippets.get(id) {
                Some(snippet) => (snippet.title.clone(), "snippet"),
                None => {
                    self.pending_delete = None;
                    return;
                }
            },
            ReferenceTarget::Container(id) => match self.workspace.containers.get(id) {
                Some(container) => (container.title.clone(), "folder"),
                None => {
                    self.pending_delete = None;
                    return;
                }
            },
        };
        let mut confirmed = false;
        let mut cancelled = false;

        egui::Window::new(())
            .id(egui::Id::new("delete-confirmation"))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.label(format!(
                    "Remove \"{title}\"?\nThis is the last reference, so the {kind} will be removed permanently."
                ));
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                    let delete_button = egui::Button::new(
                        egui::RichText::new("Remove").color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(179, 38, 30));
                    if ui.add(delete_button).clicked() {
                        confirmed = true;
                    }
                });
            });

        if confirmed {
            self.confirm_delete(pending);
        } else if cancelled {
            self.pending_delete = None;
        }
    }

    pub(super) fn render_rename_dialog(&mut self, ui: &mut egui::Ui) {
        match Self::rename_dialog_ui(ui, &mut self.rename_dialog) {
            RenameDialogResult::Confirmed(target) => {
                let new_title = self.rename_dialog.buffer.trim().to_owned();
                let ok = match &target {
                    RenameTarget::Snippet { id, .. } => self.rename_snippet(id, new_title),
                    RenameTarget::Folder { id, .. } => self.rename_folder(id, new_title),
                };
                if ok {
                    self.rename_dialog.pending = None;
                }
            }
            RenameDialogResult::Cancelled => {
                self.rename_dialog.pending = None;
            }
            RenameDialogResult::None | RenameDialogResult::Open => {}
        }
    }

    /// Renders the rename dialog only if it belongs to `ui`'s viewport, so it
    /// appears in the window that initiated the rename.
    fn rename_dialog_ui(ui: &mut egui::Ui, state: &mut RenameDialogState) -> RenameDialogResult {
        let Some(target) = state.pending.clone() else {
            return RenameDialogResult::None;
        };
        if ui.ctx().viewport_id() != target.origin() {
            return RenameDialogResult::None;
        }
        // Scope the dialog and its text-edit state to the target object so that
        // switching between objects does not carry over cursor/IME state.
        let target_key = match &target {
            RenameTarget::Snippet { id, .. } => format!("snippet:{}", id.as_str()),
            RenameTarget::Folder { id, .. } => format!("folder:{}", id.as_str()),
        };
        let mut confirmed = false;
        let mut cancelled = false;

        egui::Window::new("Rename")
            .id(egui::Id::new(("rename-dialog", target_key.clone())))
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut state.buffer)
                        .id(egui::Id::new(("rename-dialog-input", target_key.clone())))
                        .hint_text("Title"),
                );
                // Only request focus once when the dialog opens. Requesting it
                // every frame breaks IME.
                if !state.focus_requested {
                    response.request_focus();
                    state.focus_requested = true;
                }
                if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    confirmed = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                    if ui.button("OK").clicked() {
                        confirmed = true;
                    }
                });
            });

        if confirmed {
            RenameDialogResult::Confirmed(target)
        } else if cancelled {
            RenameDialogResult::Cancelled
        } else {
            RenameDialogResult::Open
        }
    }

    pub(super) fn rename_snippet(&mut self, id: &EntityId, new_title: String) -> bool {
        if new_title.is_empty()
            || self
                .all_snippets
                .values()
                .any(|snippet| snippet.id != *id && snippet.title == new_title)
        {
            return false;
        }
        let Some(snippet) = self.all_snippets.get(id) else {
            return false;
        };
        let old_title = snippet.title.clone();
        if self.store.rename(id, &old_title, &new_title).is_err() {
            return false;
        }
        if let Some(snippet) = self.all_snippets.get_mut(id) {
            snippet.title = new_title;
        }
        true
    }

    pub(super) fn rename_folder(&mut self, id: &ContainerId, new_title: String) -> bool {
        if new_title.is_empty()
            || self
                .workspace
                .containers
                .values()
                .any(|container| container.id != *id && container.title == new_title)
        {
            return false;
        }
        let Some(container) = self.workspace.containers.get_mut(id) else {
            return false;
        };
        container.title = new_title;
        let _ = self.workspace_store.save(&self.workspace);
        true
    }

    pub(super) fn open_folder(&mut self, container_id: &ContainerId) {
        if self.folder_views.contains_key(container_id) {
            return;
        }
        let canvas = Self::load_container_canvas(
            &self.workspace,
            &self.workspace_store,
            &self.all_snippets,
            container_id.clone(),
        );
        self.folder_views.insert(container_id.clone(), canvas);
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_folder_viewport(
        ui: &mut egui::Ui,
        canvas: &mut ContainerCanvas,
        title: &str,
        workspace: &mut Workspace,
        workspace_store: &WorkspaceStore,
        snippet_store: &SnippetStore,
        snippets: &mut BTreeMap<EntityId, Snippet>,
        rename_dialog: &mut RenameDialogState,
        clipboard: &mut Option<ClipboardEntry>,
    ) -> Vec<CanvasCommand> {
        let container_id = canvas.container_id.clone();
        ui.show_viewport_immediate(
            egui::ViewportId::from_hash_of(("folder-view", container_id.as_str())),
            egui::ViewportBuilder::default()
                .with_title(format!("{title} - FloatDea"))
                .with_inner_size([640.0, 480.0]),
            |child_ui, _| {
                let mut data =
                    CanvasData::new(snippets, workspace, workspace_store, snippet_store, clipboard);
                let mut commands = Self::render_canvas_panel(child_ui, canvas, &mut data);
                if child_ui.input(|input| input.viewport().close_requested()) {
                    commands.push(CanvasCommand::CloseFolder(container_id.clone()));
                }
                // Render the rename dialog inside this viewport so that it
                // appears in the folder window that initiated the rename.
                match Self::rename_dialog_ui(child_ui, rename_dialog) {
                    RenameDialogResult::Confirmed(target) => {
                        commands.push(CanvasCommand::ApplyRename(target));
                    }
                    RenameDialogResult::Cancelled => {
                        rename_dialog.pending = None;
                    }
                    RenameDialogResult::None | RenameDialogResult::Open => {}
                }
                commands
            },
        )
    }

    fn render_canvas_panel(
        ui: &mut egui::Ui,
        canvas: &mut ContainerCanvas,
        data: &mut CanvasData<'_>,
    ) -> Vec<CanvasCommand> {
        let mut commands = Vec::new();
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .inner_margin(egui::Margin::same(8))
                    .fill(ui.visuals().panel_fill),
            )
            .show(ui, |ui| {
                egui::ScrollArea::both()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        Self::render_container_canvas(ui, canvas, data, &mut commands);
                    });
            });
        render_clipboard_status(ui, data);
        commands
    }

    fn render_container_canvas(
        ui: &mut egui::Ui,
        canvas: &mut ContainerCanvas,
        data: &mut CanvasData<'_>,
        commands: &mut Vec<CanvasCommand>,
    ) {
        let available = ui.available_size();
        // Layout texts once per frame: the canvas-local rect is used for the
        // extent and overlap detection, the galley is reused for painting.
        let text_layouts: Vec<TextLayout> = {
            let painter = ui.painter();
            let text_color = ui.visuals().text_color();
            canvas
                .texts
                .iter()
                .map(|text_item| {
                    let galley = layout_text(painter, &text_item.text, TEXT_NO_WRAP, text_color);
                    let size = text_box_size(&text_item.text, &galley);
                    TextLayout {
                        rect: egui::Rect::from_min_size(
                            egui::pos2(text_item.position[0], text_item.position[1]),
                            size,
                        ),
                        galley,
                    }
                })
                .collect()
        };
        let extent = canvas
            .items
            .iter()
            .filter(|item| item_label(item, data.snippets, data.workspace).is_some())
            .fold(egui::Vec2::ZERO, |mut extent, item| {
                extent.x = extent.x.max(item.position[0] + item.size.x);
                extent.y = extent.y.max(item.position[1] + item.size.y);
                extent
            });
        let extent = text_layouts.iter().fold(extent, |mut extent, layout| {
            extent.x = extent.x.max(layout.rect.max.x);
            extent.y = extent.y.max(layout.rect.max.y);
            extent
        });
        let canvas_size = available.max(extent + egui::vec2(CANVAS_MARGIN, CANVAS_MARGIN));
        let (canvas_rect, canvas_response) =
            ui.allocate_exact_size(canvas_size, egui::Sense::click());
        let painter = ui.painter();

        painter.rect_filled(canvas_rect, 0.0, ui.visuals().panel_fill);
        paint_grid(
            painter,
            canvas_rect,
            ui.visuals().weak_text_color().gamma_multiply(0.12),
        );

        let pointer_pos = ui.input(|input| input.pointer.interact_pos());
        let mut pointer_over_card = false;
        let mut dragged = None;
        // Capture the right-click position before the context menu opens: once
        // the menu is shown, the pointer moves onto the menu items.
        let mut menu_anchor = None;
        if canvas_response.secondary_clicked() {
            menu_anchor = pointer_pos;
        }

        for index in 0..canvas.items.len() {
            let Some((title, is_folder)) =
                item_label(&canvas.items[index], data.snippets, data.workspace)
            else {
                continue;
            };
            let item = &canvas.items[index];
            let label = if is_folder {
                format!("📁 {title}")
            } else {
                title
            };
            let galley = layout_title(
                painter,
                &label,
                CARD_WIDTH - 2.0 * CARD_PADDING_H,
                ui.visuals().text_color(),
            );
            let card_size = egui::vec2(CARD_WIDTH, galley.size().y + 2.0 * CARD_MARGIN_Y);
            let rect = egui::Rect::from_min_size(
                canvas_rect.min + egui::vec2(item.position[0], item.position[1]),
                card_size,
            );
            canvas.items[index].size = card_size;
            pointer_over_card |= pointer_pos.is_some_and(|position| rect.contains(position));

            let response = ui.interact(
                rect,
                egui::Id::new((
                    "canvas-card",
                    canvas.container_id.as_str(),
                    canvas.items[index].reference_id.as_str(),
                )),
                egui::Sense::click_and_drag(),
            );
            let target = canvas.items[index].target.clone();
            let reference_id = canvas.items[index].reference_id.clone();

            response.context_menu(|ui| {
                let origin = ui.ctx().viewport_id();
                if ui.button("Link").clicked() {
                    *data.clipboard = Some(ClipboardEntry {
                        source_container: canvas.container_id.clone(),
                        reference_id: reference_id.clone(),
                        target: target.clone(),
                        semantics: ClipboardSemantics::Link,
                        origin,
                    });
                    ui.close();
                }
                if ui.button("Move").clicked() {
                    *data.clipboard = Some(ClipboardEntry {
                        source_container: canvas.container_id.clone(),
                        reference_id: reference_id.clone(),
                        target: target.clone(),
                        semantics: ClipboardSemantics::Move,
                        origin,
                    });
                    ui.close();
                }
                ui.separator();
                match &target {
                    ReferenceTarget::Snippet(entity_id) => {
                        if ui.button("Rename").clicked() {
                            commands.push(CanvasCommand::RenameSnippet(entity_id.clone()));
                            ui.close();
                        }
                    }
                    ReferenceTarget::Container(container_id) => {
                        if ui.button("Rename").clicked() {
                            commands.push(CanvasCommand::RenameFolder(container_id.clone()));
                            ui.close();
                        }
                    }
                }
                let last_link = reference_count(data.workspace, &target) == 1;
                if ui.button(if last_link { "Delete" } else { "Unlink" }).clicked() {
                    commands.push(CanvasCommand::DeleteReference {
                        owner: canvas.container_id.clone(),
                        reference: reference_id.clone(),
                        target: target.clone(),
                    });
                    ui.close();
                }
            });

            if response.clicked() {
                commands.push(match target {
                    ReferenceTarget::Snippet(id) => CanvasCommand::OpenSnippet(id),
                    ReferenceTarget::Container(id) => CanvasCommand::OpenFolder(id),
                });
            }
            if response.drag_started() {
                canvas.dragging = Some(DragState {
                    index,
                    start_position: canvas.items[index].position,
                    invalid: false,
                });
            }
            if canvas.dragging.is_some_and(|drag| drag.index == index) && response.dragged() {
                dragged = Some((index, ui.input(|input| input.pointer.delta())));
            }

            paint_card(
                painter,
                rect,
                &galley,
                canvas.dragging.is_some_and(|drag| drag.index == index),
                is_folder,
                ui.visuals(),
            );
        }

        if let Some((index, delta)) = dragged {
            let item = &mut canvas.items[index];
            item.position[0] = (item.position[0] + delta.x).max(0.0);
            item.position[1] = (item.position[1] + delta.y).max(0.0);
            let rect = egui::Rect::from_min_size(
                egui::pos2(item.position[0], item.position[1]),
                item.size,
            );
            let invalid = overlaps(
                canvas,
                Some(index),
                None,
                rect,
                data.snippets,
                data.workspace,
                &text_layouts,
            );
            if let Some(drag) = &mut canvas.dragging {
                drag.invalid = invalid;
            }
        }

        if canvas.dragging.is_some_and(|drag| drag.invalid) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        }
        if canvas.dragging.is_some() && !ui.input(|input| input.pointer.any_down()) {
            let drag = canvas.dragging.take().expect("drag state disappeared");
            if drag.invalid {
                canvas.items[drag.index].position = drag.start_position;
            } else {
                canvas.save_layout(data.workspace_store);
            }
        }

        let pointer_over_text =
            render_canvas_texts(ui, canvas, data, canvas_rect, &text_layouts, pointer_pos);

        if !pointer_over_card && !pointer_over_text {
            canvas_response.context_menu(|ui| {
                if let Some(entry) = data.clipboard.as_ref() {
                    let valid = clipboard_valid_for(
                        entry,
                        &canvas.container_id,
                        data.snippets,
                        data.workspace,
                    );
                    let label = match &entry.target {
                        ReferenceTarget::Snippet(id) => data
                            .snippets
                            .get(id)
                            .map(|snippet| snippet.title.as_str())
                            .unwrap_or("?"),
                        ReferenceTarget::Container(id) => data
                            .workspace
                            .containers
                            .get(id)
                            .map(|container| container.title.as_str())
                            .unwrap_or("?"),
                    };
                    let verb = match entry.semantics {
                        ClipboardSemantics::Link => "Paste (Link)",
                        ClipboardSemantics::Move => "Paste (Move)",
                    };
                    let button = egui::Button::new(format!("{verb}: {label}"));
                    let clicked = if valid {
                        ui.add(button).clicked()
                    } else {
                        ui.add_enabled(false, button).clicked()
                    };
                    if clicked {
                        commands.push(CanvasCommand::PasteClipboard {
                            container: canvas.container_id.clone(),
                            entry: entry.clone(),
                        });
                        ui.close();
                    }
                    ui.separator();
                }
                if ui.button("New Snippet").clicked() {
                    create_snippet(canvas, data);
                    ui.close();
                }
                if ui.button("New Folder").clicked() {
                    create_folder(canvas, data);
                    ui.close();
                }
                if ui.button("New Text").clicked() {
                    let position = menu_anchor
                        .map(|pos| {
                            [
                                (pos.x - canvas_rect.min.x).max(0.0),
                                (pos.y - canvas_rect.min.y).max(0.0),
                            ]
                        })
                        .unwrap_or_else(|| default_text_position(canvas));
                    create_text(canvas, data, position);
                    ui.close();
                }
                if ui.button("Organize").clicked() {
                    canvas.organize(data.workspace_store, available.y);
                    ui.close();
                }
            });
        }
    }
}

/// Renders the canvas texts (interaction, drag, context menu, editing) on top
/// of the cards. Returns whether the pointer is over any text.
fn render_canvas_texts(
    ui: &mut egui::Ui,
    canvas: &mut ContainerCanvas,
    data: &mut CanvasData<'_>,
    canvas_rect: egui::Rect,
    text_layouts: &[TextLayout],
    pointer_pos: Option<egui::Pos2>,
) -> bool {
    let mut pointer_over_text = false;

    for (index, layout) in text_layouts.iter().enumerate() {
        let rect = layout.rect.translate(canvas_rect.min.to_vec2());
        let text_id = canvas.texts[index].id.clone();
        pointer_over_text |= pointer_pos.is_some_and(|position| rect.contains(position));

        let response = ui.interact(
            rect,
            egui::Id::new(("canvas-text", canvas.container_id.as_str(), text_id.as_str())),
            egui::Sense::click_and_drag(),
        );

        if response.double_clicked() {
            canvas.editing_text = Some(text_id.clone());
            canvas.edit_focus_requested = false;
        }
        if response.drag_started() {
            canvas.dragging_text = Some((index, canvas.texts[index].position, false));
        }
        if canvas.dragging_text.is_some_and(|drag| drag.0 == index) && response.dragged() {
            let delta = ui.input(|input| input.pointer.delta());
            canvas.texts[index].position[0] =
                (canvas.texts[index].position[0] + delta.x).max(0.0);
            canvas.texts[index].position[1] =
                (canvas.texts[index].position[1] + delta.y).max(0.0);
            let drag_rect = egui::Rect::from_min_size(
                egui::pos2(canvas.texts[index].position[0], canvas.texts[index].position[1]),
                layout.rect.size(),
            );
            let invalid = overlaps(
                canvas,
                None,
                Some(index),
                drag_rect,
                data.snippets,
                data.workspace,
                text_layouts,
            );
            if let Some(drag) = &mut canvas.dragging_text {
                drag.2 = invalid;
            }
        }
        if canvas.dragging_text.is_some_and(|drag| drag.2) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        }

        response.context_menu(|ui| {
            if ui.button("Edit").clicked() {
                canvas.editing_text = Some(text_id.clone());
                canvas.edit_focus_requested = false;
                ui.close();
            }
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(canvas.texts[index].text.clone());
                ui.close();
            }
            if ui.button("Delete Text").clicked() {
                delete_text(canvas, data, &text_id);
                ui.close();
            }
        });

        // Shared background: display and edit look identical.
        {
            let painter = ui.painter();
            paint_text_bg(painter, rect, ui.visuals());
        }

        if canvas.editing_text.as_ref() == Some(&text_id) {
            let escaped = ui.input(|input| input.key_pressed(egui::Key::Escape));
            let clicked_outside = ui.input(|input| input.pointer.any_pressed())
                && ui
                    .input(|input| input.pointer.interact_pos())
                    .is_some_and(|position| !rect.contains(position));
            if escaped || clicked_outside {
                canvas.editing_text = None;
                canvas.save_layout(data.workspace_store);
            } else {
                let container_id = canvas.container_id.clone();
                let edit_rect = rect.shrink2(egui::vec2(TEXT_PADDING_H, TEXT_PADDING_V));
                let desired_rows = (canvas.texts[index].text.lines().count() + 1).clamp(1, 8);
                let text_edit = ui.put(
                    edit_rect,
                    egui::TextEdit::multiline(&mut canvas.texts[index].text)
                        .id(egui::Id::new((
                            "canvas-text-edit",
                            container_id.as_str(),
                            text_id.as_str(),
                        )))
                        .desired_width(edit_rect.width())
                        .desired_rows(desired_rows)
                        .font(egui::FontId::proportional(15.0))
                        .frame(egui::Frame::NONE),
                );
                if !canvas.edit_focus_requested {
                    text_edit.request_focus();
                    canvas.edit_focus_requested = true;
                }
            }
        } else {
            let painter = ui.painter();
            paint_text_galley(painter, rect, &layout.galley, ui.visuals());
        }
    }

    if canvas.dragging_text.is_some() && !ui.input(|input| input.pointer.any_down()) {
        let drag = canvas.dragging_text.take().expect("drag state disappeared");
        if drag.2 {
            canvas.texts[drag.0].position = drag.1;
        }
        canvas.save_layout(data.workspace_store);
    }

    pointer_over_text
}

fn item_label(
    item: &CanvasItem,
    snippets: &BTreeMap<EntityId, Snippet>,
    workspace: &Workspace,
) -> Option<(String, bool)> {
    let (title, is_folder) = match &item.target {
        ReferenceTarget::Snippet(id) => (&snippets.get(id)?.title, false),
        ReferenceTarget::Container(id) => (&workspace.containers.get(id)?.title, true),
    };
    (!title.is_empty()).then(|| (title.clone(), is_folder))
}

fn unique_title(base: &str, mut exists: impl FnMut(&str) -> bool) -> String {
    for number in 1_u64.. {
        let candidate = if number == 1 {
            base.to_owned()
        } else {
            format!("{base} {number}")
        };
        if !exists(&candidate) {
            return candidate;
        }
    }
    unreachable!("the title counter cannot be exhausted")
}

fn create_snippet(canvas: &mut ContainerCanvas, data: &mut CanvasData<'_>) {
    let title = unique_title("Untitled", |candidate| {
        data.snippets
            .values()
            .any(|snippet| snippet.title == candidate)
    });
    let position = canvas.default_position(data);
    let snippet = Snippet {
        id: EntityId::new(),
        title,
        content: String::new(),
    };
    let _ = data.snippet_store.save(&snippet);
    let Ok(reference_id) = data
        .workspace
        .add_snippet_reference(&canvas.container_id, snippet.id.clone())
    else {
        return;
    };
    let _ = data.workspace_store.save(data.workspace);
    canvas.items.push(CanvasItem {
        reference_id: reference_id.clone(),
        target: ReferenceTarget::Snippet(snippet.id.clone()),
        position,
        size: egui::vec2(CARD_WIDTH, 25.0),
    });
    canvas.layout.items.insert(
        reference_id,
        CardLayout {
            position,
            color: None,
        },
    );
    let _ = data.workspace_store.save_layout(&canvas.layout);
    data.snippets.insert(snippet.id.clone(), snippet);
}

fn create_folder(canvas: &mut ContainerCanvas, data: &mut CanvasData<'_>) {
    let title = unique_title("New Folder", |candidate| {
        data.workspace
            .containers
            .values()
            .any(|container| container.title == candidate)
    });
    let position = canvas.default_position(data);
    let container_id = data.workspace.create_container(title);
    let Ok(reference_id) = data
        .workspace
        .add_container_reference(&canvas.container_id, container_id.clone())
    else {
        return;
    };
    let _ = data.workspace_store.save(data.workspace);
    canvas.items.push(CanvasItem {
        reference_id: reference_id.clone(),
        target: ReferenceTarget::Container(container_id),
        position,
        size: egui::vec2(CARD_WIDTH, 25.0),
    });
    canvas.layout.items.insert(
        reference_id,
        CardLayout {
            position,
            color: None,
        },
    );
    let _ = data.workspace_store.save_layout(&canvas.layout);
}

/// Finds the first free default grid slot for a new card, skipping positions
/// already occupied by visible items. Shared between the canvas and the
/// clipboard paste path.
pub(super) fn default_position_for(
    items: &[CanvasItem],
    snippets: &BTreeMap<EntityId, Snippet>,
    workspace: &Workspace,
    text_rects: &[egui::Rect],
) -> [f32; 2] {
    for index in 0..640 {
        let position = default_card_position(index);
        let candidate = egui::Rect::from_min_size(
            egui::pos2(position[0], position[1]),
            egui::vec2(CARD_WIDTH, 40.0),
        );
        let occupied = items.iter().any(|item| {
            item_label(item, snippets, workspace).is_some()
                && egui::Rect::from_min_size(
                    egui::pos2(item.position[0], item.position[1]),
                    item.size,
                )
                .intersects(candidate)
        }) || text_rects.iter().any(|text_rect| text_rect.intersects(candidate));
        if !occupied {
            return position;
        }
    }
    [24.0, 24.0]
}

/// Whether pasting `entry` into `container` is currently allowed. The `Paste`
/// menu button is disabled when this is `false`.
pub(super) fn clipboard_valid_for(
    entry: &ClipboardEntry,
    container: &ContainerId,
    snippets: &BTreeMap<EntityId, Snippet>,
    workspace: &Workspace,
) -> bool {
    if matches!(entry.semantics, ClipboardSemantics::Move) && &entry.source_container == container {
        return false;
    }
    match &entry.target {
        ReferenceTarget::Snippet(entity_id) => snippets.contains_key(entity_id),
        ReferenceTarget::Container(target_id) => {
            workspace.containers.contains_key(target_id) && target_id != container
        }
    }
}

/// Renders the clipboard status indicator in the viewport that originated the
/// current clipboard entry (the window where `Link`/`Move` was chosen).
fn render_clipboard_status(ui: &mut egui::Ui, data: &mut CanvasData<'_>) {
    let Some(entry) = data.clipboard.clone() else {
        return;
    };
    if entry.origin != ui.ctx().viewport_id() {
        return;
    }
    let title = match &entry.target {
        ReferenceTarget::Snippet(id) => data
            .snippets
            .get(id)
            .map(|snippet| snippet.title.clone())
            .unwrap_or_else(|| "?".to_owned()),
        ReferenceTarget::Container(id) => data
            .workspace
            .containers
            .get(id)
            .map(|container| container.title.clone())
            .unwrap_or_else(|| "?".to_owned()),
    };
    let verb = match entry.semantics {
        ClipboardSemantics::Link => "Link",
        ClipboardSemantics::Move => "Move",
    };
    let text = format!("Clipboard: {title} ({verb}) — right-click a canvas → Paste");
    let mut clear_clipboard = false;
    egui::Window::new("clipboard-status")
        .id(egui::Id::new("clipboard-status-window"))
        .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(8.0, -8.0))
        .collapsible(false)
        .resizable(false)
        .title_bar(false)
        .show(ui.ctx(), |ui| {
            ui.label(text);
            if ui.button("Clear clipboard").clicked() {
                clear_clipboard = true;
            }
        });
    if clear_clipboard {
        *data.clipboard = None;
    }
}

fn paint_grid(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    const STEP: f32 = 32.0;
    let mut y = rect.min.y + STEP;
    while y < rect.max.y {
        let mut x = rect.min.x + STEP;
        while x < rect.max.x {
            painter.circle_filled(egui::pos2(x, y), 1.0, color);
            x += STEP;
        }
        y += STEP;
    }
}

fn layout_title(
    painter: &egui::Painter,
    text: &str,
    max_width: f32,
    color: egui::Color32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(14.0),
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = max_width.max(10.0);
    painter.layout_job(job)
}

fn paint_card(
    painter: &egui::Painter,
    rect: egui::Rect,
    galley: &std::sync::Arc<egui::Galley>,
    dragging: bool,
    is_folder: bool,
    visuals: &egui::Visuals,
) {
    let bg = if dragging {
        visuals.widgets.active.bg_fill
    } else if is_folder {
        visuals.widgets.hovered.bg_fill.gamma_multiply(0.7)
    } else {
        visuals.widgets.inactive.bg_fill
    };
    let stroke = if dragging {
        egui::Stroke::new(2.0, visuals.selection.stroke.color)
    } else if is_folder {
        egui::Stroke::new(1.5, visuals.selection.stroke.color.gamma_multiply(0.65))
    } else {
        egui::Stroke::new(1.0, visuals.widgets.inactive.bg_stroke.color)
    };
    painter.rect(rect, 2.5, bg, stroke, egui::StrokeKind::Inside);

    painter.with_clip_rect(rect).galley(
        rect.min + egui::vec2(CARD_PADDING_H, CARD_MARGIN_Y),
        galley.clone(),
        visuals.text_color(),
    );
}

const TEXT_PADDING_H: f32 = 8.0;
const TEXT_PADDING_V: f32 = 6.0;
/// No line-width cap: the box widens to fit its content (only the scroll area
/// bounds it).
const TEXT_NO_WRAP: f32 = 10_000.0;
/// Small floor so an empty or one-character annotation stays visible and
/// clickable; the box otherwise shrinks to hug its content.
const TEXT_MIN_WIDTH: f32 = 60.0;
const TEXT_MIN_HEIGHT: f32 = 28.0;
/// Fixed guess used only for placement avoidance, not a wrap limit.
const TEXT_APPROX_WIDTH: f32 = 180.0;

/// Precomputed layout for one canvas text: its canvas-local rect and the
/// galley reused for painting.
struct TextLayout {
    rect: egui::Rect,
    galley: std::sync::Arc<egui::Galley>,
}

/// Box size for a text galley: hugs the content (no wrap limit), with a small
/// floor so an empty or one-character annotation stays visible and clickable.
fn text_box_size(text: &str, galley: &std::sync::Arc<egui::Galley>) -> egui::Vec2 {
    if text.is_empty() {
        return egui::vec2(TEXT_MIN_WIDTH, TEXT_MIN_HEIGHT);
    }
    egui::vec2(
        (galley.size().x + 2.0 * TEXT_PADDING_H).max(TEXT_MIN_WIDTH),
        (galley.size().y + 2.0 * TEXT_PADDING_V).max(TEXT_MIN_HEIGHT),
    )
}

fn layout_text(
    painter: &egui::Painter,
    text: &str,
    max_width: f32,
    color: egui::Color32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(15.0),
            color,
            ..Default::default()
        },
    );
    job.wrap.max_width = max_width.max(10.0);
    painter.layout_job(job)
}

/// Paints the teal rounded background shared by display and edit modes.
fn paint_text_bg(painter: &egui::Painter, rect: egui::Rect, visuals: &egui::Visuals) {
    let bg = if visuals.dark_mode {
        egui::Color32::from_rgba_unmultiplied(0, 92, 92, 220)
    } else {
        egui::Color32::from_rgb(214, 244, 242)
    };
    painter.rect_filled(rect, 3.0, bg);
}

/// Paints the annotation text on top of the teal background.
fn paint_text_galley(
    painter: &egui::Painter,
    rect: egui::Rect,
    galley: &std::sync::Arc<egui::Galley>,
    visuals: &egui::Visuals,
) {
    let text_color = if visuals.dark_mode {
        egui::Color32::from_gray(230)
    } else {
        egui::Color32::from_gray(30)
    };
    painter.with_clip_rect(rect).galley(
        rect.min + egui::vec2(TEXT_PADDING_H, TEXT_PADDING_V),
        galley.clone(),
        text_color,
    );
}

pub(super) fn create_text(
    canvas: &mut ContainerCanvas,
    data: &mut CanvasData<'_>,
    position: [f32; 2],
) {
    let id = TextId::new();
    canvas.texts.push(CanvasText {
        id: id.clone(),
        position,
        text: String::new(),
        color: None,
    });
    // Start editing the new text immediately so the user can type right away.
    canvas.editing_text = Some(id);
    canvas.edit_focus_requested = false;
    canvas.save_layout(data.workspace_store);
}

pub(super) fn delete_text(
    canvas: &mut ContainerCanvas,
    data: &mut CanvasData<'_>,
    text_id: &TextId,
) {
    canvas.texts.retain(|text| &text.id != text_id);
    if canvas.editing_text.as_ref() == Some(text_id) {
        canvas.editing_text = None;
    }
    canvas.save_layout(data.workspace_store);
}

fn card_rect(item: &CanvasItem) -> egui::Rect {
    egui::Rect::from_min_size(egui::pos2(item.position[0], item.position[1]), item.size)
}

/// Whether `rect` overlaps any card or any text, excluding the dragged element
/// itself (`card_index` / `text_index`).
fn overlaps(
    canvas: &ContainerCanvas,
    card_index: Option<usize>,
    text_index: Option<usize>,
    rect: egui::Rect,
    snippets: &BTreeMap<EntityId, Snippet>,
    workspace: &Workspace,
    text_layouts: &[TextLayout],
) -> bool {
    let card_overlap = canvas.items.iter().enumerate().any(|(index, item)| {
        Some(index) != card_index
            && item_label(item, snippets, workspace).is_some()
            && card_rect(item).intersects(rect)
    });
    let text_overlap = text_layouts
        .iter()
        .enumerate()
        .any(|(index, layout)| Some(index) != text_index && layout.rect.intersects(rect));
    card_overlap || text_overlap
}

/// Approximate text rects (fixed size, no text layout needed). Used to keep
/// newly placed cards and fallback text positions clear of existing texts.
pub(super) fn approx_text_rects(texts: &[CanvasText]) -> Vec<egui::Rect> {
    texts
        .iter()
        .map(|text| {
            egui::Rect::from_min_size(
                egui::pos2(text.position[0], text.position[1]),
                egui::vec2(TEXT_APPROX_WIDTH, TEXT_MIN_HEIGHT),
            )
        })
        .collect()
}

fn default_text_position(canvas: &ContainerCanvas) -> [f32; 2] {
    let text_rects = approx_text_rects(&canvas.texts);
    for index in 0..640 {
        let position = default_card_position(index);
        let candidate = egui::Rect::from_min_size(
            egui::pos2(position[0], position[1]),
            egui::vec2(TEXT_APPROX_WIDTH, TEXT_MIN_HEIGHT),
        );
        let occupied = canvas.items.iter().any(|item| {
            egui::Rect::from_min_size(
                egui::pos2(item.position[0], item.position[1]),
                item.size,
            )
            .intersects(candidate)
        }) || text_rects.iter().any(|rect| rect.intersects(candidate));
        if !occupied {
            return position;
        }
    }
    [24.0, 24.0]
}
