use super::*;

pub(super) struct CanvasData<'a> {
    snippets: &'a mut BTreeMap<EntityId, Snippet>,
    workspace: &'a mut Workspace,
    workspace_store: &'a WorkspaceStore,
    snippet_store: &'a SnippetStore,
    clipboard: &'a mut Option<ClipboardEntry>,
    /// AI sidecar state cache: conversation titles and per-box data used to
    /// render conversation cards inside AI boxes.
    ai: &'a BTreeMap<ContainerId, AiBoxData>,
    /// Snap dragged cards and drop positions to the canvas grid (from settings).
    snap_to_grid: bool,
    /// Whether the dot grid is drawn on the canvas (from settings).
    show_grid: bool,
    /// Whether this is the root canvas. In floating-window mode the clipboard
    /// status indicator is only shown on the root canvas: every floating window
    /// shares the root viewport, so showing it in each canvas would duplicate it.
    is_root: bool,
    /// Whether the app presents snippet/folder windows as floating windows
    /// inside the main window (full-window mode) instead of native OS windows.
    floating: bool,
    /// Per-frame guard (reset by `HomePage::ui_impl`): an OS-level file drop is
    /// a single frame-global `dropped_files` event shared by every canvas of
    /// the viewport; exactly one canvas consumes it. Shared across canvases so
    /// a drop aimed at one window never inserts cards in another.
    os_file_drop_consumed: &'a mut bool,
    /// Hover-to-preview state, shared across all canvases (only one preview
    /// is active at a time).
    hover_preview: &'a mut HoverPreview,
}

impl<'a> CanvasData<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        snippets: &'a mut BTreeMap<EntityId, Snippet>,
        workspace: &'a mut Workspace,
        workspace_store: &'a WorkspaceStore,
        snippet_store: &'a SnippetStore,
        clipboard: &'a mut Option<ClipboardEntry>,
        ai: &'a BTreeMap<ContainerId, AiBoxData>,
        snap_to_grid: bool,
        show_grid: bool,
        is_root: bool,
        floating: bool,
        os_file_drop_consumed: &'a mut bool,
        hover_preview: &'a mut HoverPreview,
    ) -> Self {
        Self {
            snippets,
            workspace,
            workspace_store,
            snippet_store,
            clipboard,
            ai,
            snap_to_grid,
            show_grid,
            is_root,
            floating,
            os_file_drop_consumed,
            hover_preview,
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

/// Outcome of rendering the delete confirmation dialog for a specific
/// viewport.
enum DeleteDialogResult {
    /// No dialog is pending, or it belongs to another viewport.
    None,
    /// The dialog is open and awaiting input.
    Open,
    /// The user confirmed the deletion.
    Confirmed(PendingDelete),
    /// The user cancelled.
    Cancelled,
}

impl ContainerCanvas {
    fn default_position(&self, data: &CanvasData<'_>) -> [f32; 2] {
        default_position_for(
            &self.container_id,
            &self.items,
            data.snippets,
            data.workspace,
            data.ai,
            &approx_text_rects(&self.texts),
        )
    }
}

impl HomePage {
    pub(super) fn render_home_panel(&mut self, ui: &mut egui::Ui) -> Vec<CanvasCommand> {
        let floating = self.settings.window_mode == WindowMode::Floating;
        let mut data = CanvasData::new(
            &mut self.all_snippets,
            &mut self.workspace,
            &self.workspace_store,
            &self.store,
            &mut self.clipboard,
            &self.ai_boxes,
            self.settings.snap_to_grid,
            self.settings.show_grid,
            true,
            floating,
            &mut self.os_file_drop_consumed,
            &mut self.hover_preview,
        );
        if floating {
            // Full-window mode: the root box is a normal draggable/resizable
            // `egui::Window` sized 640×480 by default, floating over the larger
            // main window. Its close button pops an "Exit?" confirmation
            // (rendered by [`Self::render_root_exit_dialog`]) instead of
            // closing the window, so it always stays visible until confirmed.
            let mut open = true;
            let commands = egui::Window::new("Root - FloatDea")
                .id(egui::Id::new("root-window"))
                .open(&mut open)
                .default_pos(egui::pos2(8.0, 8.0))
                .default_size(egui::vec2(ROOT_CANVAS_SIZE[0], ROOT_CANVAS_SIZE[1]))
                .show(ui.ctx(), |ui| {
                    Self::render_canvas_panel(ui, &mut self.root, &mut data)
                })
                .and_then(|inner_response| inner_response.inner)
                .unwrap_or_default();
            if !open && !self.root_exit_pending {
                self.root_exit_pending = true;
            }
            commands
        } else {
            // Native mode: the root box fills the main window.
            Self::render_canvas_panel(ui, &mut self.root, &mut data)
        }
    }

    pub(super) fn render_delete_dialog(&mut self, ui: &mut egui::Ui) {
        match Self::delete_dialog_ui(
            ui,
            &mut self.pending_delete,
            &self.all_snippets,
            &self.workspace,
        ) {
            DeleteDialogResult::Confirmed(pending) => self.confirm_delete(pending),
            DeleteDialogResult::Cancelled => self.pending_delete = None,
            DeleteDialogResult::None | DeleteDialogResult::Open => {}
        }
    }

    /// Renders the delete confirmation dialog only if it belongs to `ui`'s
    /// viewport, so it appears in the window that initiated the delete.
    fn delete_dialog_ui(
        ui: &mut egui::Ui,
        pending: &mut Option<PendingDelete>,
        snippets: &BTreeMap<EntityId, Snippet>,
        workspace: &Workspace,
    ) -> DeleteDialogResult {
        let Some(target) = pending.clone() else {
            return DeleteDialogResult::None;
        };
        if ui.ctx().viewport_id() != target.origin {
            return DeleteDialogResult::None;
        }
        let (title, kind) = match &target.target {
            ReferenceTarget::Snippet(id) => match snippets.get(id) {
                Some(snippet) => (snippet.title.clone(), "snippet"),
                None => {
                    *pending = None;
                    return DeleteDialogResult::None;
                }
            },
            ReferenceTarget::Container(id) => match workspace.containers.get(id) {
                Some(container) => (container.title.clone(), "folder"),
                None => {
                    *pending = None;
                    return DeleteDialogResult::None;
                }
            },
            // Special items cannot be deleted; the dialog never opens for them.
            ReferenceTarget::Special(_) => {
                *pending = None;
                return DeleteDialogResult::None;
            }
            // Conversation cards are removed by the AI layer, never through the
            // entity-delete confirmation.
            ReferenceTarget::Conversation(_) => {
                *pending = None;
                return DeleteDialogResult::None;
            }
            // External-file cards are deleted without confirmation (the file on
            // disk is never touched), so the dialog never opens for them.
            ReferenceTarget::ExternalFile(_) => {
                *pending = None;
                return DeleteDialogResult::None;
            }
        };
        let mut confirmed = false;
        let mut cancelled = false;

        egui::Window::new(())
            .id(egui::Id::new(("delete-confirmation", target.origin)))
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
            DeleteDialogResult::Confirmed(target)
        } else if cancelled {
            DeleteDialogResult::Cancelled
        } else {
            DeleteDialogResult::Open
        }
    }

    pub(super) fn render_rename_dialog(&mut self, ui: &mut egui::Ui) {
        match Self::rename_dialog_ui(ui, &mut self.rename_dialog) {
            RenameDialogResult::Confirmed(target) => {
                let new_title = self.rename_dialog.buffer.trim().to_owned();
                let ok = match &target {
                    RenameTarget::Snippet { id, .. } => self.rename_snippet(id, new_title),
                    RenameTarget::Folder { id, .. } => self.rename_folder(id, new_title),
                    RenameTarget::Conversation { ai_box, id, .. } => {
                        self.rename_conversation(ai_box, id, new_title)
                    }
                    RenameTarget::ExternalFile { id, .. } => {
                        self.rename_external_file(id, new_title)
                    }
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
            RenameTarget::Conversation { id, .. } => format!("conversation:{}", id.as_str()),
            RenameTarget::ExternalFile { id, .. } => format!("file:{}", id.as_str()),
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
        // Identity is the ulid, so duplicate titles are allowed.
        if new_title.is_empty() {
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
        // Identity is the ulid, so duplicate titles are allowed.
        if new_title.is_empty() {
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
        pending_delete: &mut Option<PendingDelete>,
        search: &mut SearchState,
        clipboard: &mut Option<ClipboardEntry>,
        external_open_error: &Option<(String, egui::ViewportId, u32)>,
        os_file_drop_consumed: &mut bool,
        ai: &BTreeMap<ContainerId, AiBoxData>,
        snap_to_grid: bool,
        show_grid: bool,
        hover_preview: &mut HoverPreview,
    ) -> Vec<CanvasCommand> {
        let container_id = canvas.container_id.clone();
        ui.show_viewport_immediate(
            egui::ViewportId::from_hash_of(("folder-view", container_id.as_str())),
            egui::ViewportBuilder::default()
                .with_title(format!("{title} - FloatDea"))
                .with_inner_size([640.0, 480.0]),
            |child_ui, _| {
                let mut data = CanvasData::new(
                    snippets,
                    workspace,
                    workspace_store,
                    snippet_store,
                    clipboard,
                    ai,
                    snap_to_grid,
                    show_grid,
                    false,
                    false,
                    os_file_drop_consumed,
                    hover_preview,
                );
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
                // Render the delete confirmation inside this viewport so that
                // it appears in the folder window that initiated the delete.
                match Self::delete_dialog_ui(child_ui, pending_delete, snippets, workspace) {
                    DeleteDialogResult::Confirmed(target) => {
                        commands.push(CanvasCommand::ConfirmDelete(target));
                    }
                    DeleteDialogResult::Cancelled => {
                        *pending_delete = None;
                    }
                    DeleteDialogResult::None | DeleteDialogResult::Open => {}
                }
                // Render the global search window inside this viewport so that it
                // appears in the folder window that initiated the search. The
                // viewport guard inside `render_search_window` skips it here
                // unless `search.origin` is exactly this folder window.
                if let Some(id) = render_search_window(child_ui, search, snippets) {
                    commands.push(CanvasCommand::OpenSnippet(id));
                }
                // Render the transient "could not open external file" toast in
                // the folder window that triggered the failed open.
                render_external_open_error(child_ui, external_open_error);
                commands
            },
        )
    }

    /// Renders a folder canvas as a floating window inside the main window
    /// (full-window mode). Same content as [`Self::render_folder_viewport`];
    /// the rename/delete dialogs are not rendered here because they float at
    /// the root viewport level (origins are the root viewport in this mode).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_folder_window(
        ui: &mut egui::Ui,
        canvas: &mut ContainerCanvas,
        title: &str,
        workspace: &mut Workspace,
        workspace_store: &WorkspaceStore,
        snippet_store: &SnippetStore,
        snippets: &mut BTreeMap<EntityId, Snippet>,
        clipboard: &mut Option<ClipboardEntry>,
        os_file_drop_consumed: &mut bool,
        ai: &BTreeMap<ContainerId, AiBoxData>,
        snap_to_grid: bool,
        show_grid: bool,
        hover_preview: &mut HoverPreview,
    ) -> Vec<CanvasCommand> {
        let container_id = canvas.container_id.clone();
        let mut open = true;
        let mut commands = egui::Window::new(format!("{title} - FloatDea"))
            .id(egui::Id::new(("folder-window", container_id.as_str())))
            .open(&mut open)
            .default_pos(egui::pos2(680.0, 60.0))
            .default_size([640.0, 480.0])
            .show(ui.ctx(), |ui| {
                let mut data = CanvasData::new(
                    snippets,
                    workspace,
                    workspace_store,
                    snippet_store,
                    clipboard,
                    ai,
                    snap_to_grid,
                    show_grid,
                    false,
                    true,
                    os_file_drop_consumed,
                    hover_preview,
                );
                Self::render_canvas_panel(ui, canvas, &mut data)
            })
            .and_then(|inner_response| inner_response.inner)
            .unwrap_or_default();
        if !open {
            commands.push(CanvasCommand::CloseFolder(container_id.clone()));
        }
        commands
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
            .filter(|item| {
                item_label(
                    &canvas.container_id,
                    item,
                    data.snippets,
                    data.workspace,
                    data.ai,
                )
                .is_some()
            })
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
        if data.show_grid {
            paint_grid(
                painter,
                canvas_rect,
                ui.visuals().weak_text_color().gamma_multiply(0.7),
            );
        }

        let pointer_pos = ui.input(|input| input.pointer.interact_pos());
        // OS-level file drag-and-drop (dragged in from the system file
        // manager): `hovered_files` is non-empty while a drag hovers over the
        // window, `dropped_files` is non-empty only on the frame of the drop
        // (egui-winit clears both every frame via `take_egui_input`). AI boxes
        // reject file drops: their members must be model-readable sources.
        //
        // During an X11 file drag the pointer is grabbed by the source window,
        // so the app typically receives **no pointer-motion** while the file
        // hovers — only the `HoveredFile`/`DroppedFile` events. The drop is
        // therefore attributed without requiring a live pointer position:
        // in native mode each viewport holds exactly one canvas (the right one
        // receives the event), and a per-frame shared guard (`CanvasData`)
        // makes sure the frame-global drop is consumed exactly once.
        let hovered_files = ui.input(|input| input.raw.hovered_files.clone());
        let dropped_files = ui.input(|input| input.raw.dropped_files.clone());
        let canvas_accepts_files = !data.workspace.is_ai_box(&canvas.container_id);
        let files_hovering = !hovered_files.is_empty();
        // `latest_pos` is the last known pointer position — stale during an X11
        // file drag, so it only drives the drop position and the hover preview,
        // never whether the drop is accepted.
        let file_pointer = ui.input(|input| input.pointer.latest_pos());
        let pointer_over_canvas = file_pointer.is_some_and(|pointer| canvas_rect.contains(pointer));
        // Drop position only when the pointer is actually over the canvas;
        // otherwise (stale/absent pointer during the drag) fall back to a free
        // default slot so the card never lands off-canvas.
        let file_drop_pos = if pointer_over_canvas {
            file_pointer.map(|pointer| {
                let mut position = [
                    (pointer.x - canvas_rect.min.x).max(0.0),
                    (pointer.y - canvas_rect.min.y).max(0.0),
                ];
                if data.snap_to_grid {
                    position = snap_position(position);
                }
                position
            })
        } else {
            None
        };
        if files_hovering {
            // Keep repainting so the highlight stays visible while dragging.
            ui.ctx().request_repaint();
            if canvas_accepts_files && (pointer_over_canvas || !data.floating) {
                // Highlight the drop target and preview a card near the pointer
                // (or the canvas center when the pointer position is unknown).
                let accent = ui.visuals().selection.stroke.color;
                painter.rect_stroke(
                    canvas_rect.expand(2.0),
                    4.0,
                    egui::Stroke::new(2.0, accent.gamma_multiply(0.9)),
                    egui::StrokeKind::Outside,
                );
                let anchor = file_pointer.unwrap_or(canvas_rect.center());
                let preview = egui::Rect::from_center_size(anchor, egui::vec2(CARD_WIDTH, 34.0));
                painter.rect_stroke(
                    preview,
                    2.5,
                    egui::Stroke::new(1.5, accent),
                    egui::StrokeKind::Inside,
                );
                let galley = layout_title(painter, "Drop to insert file", 150.0, accent);
                let label_rect =
                    egui::Rect::from_min_size(preview.max + egui::vec2(6.0, 4.0), galley.size());
                painter.rect_filled(label_rect.expand(3.0), 3.0, ui.visuals().panel_fill);
                painter.galley(label_rect.min, galley, accent);
            } else {
                ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
            }
        }
        // A card drag in any canvas publishes an egui drag-and-drop payload;
        // this canvas may be a drop target while the pointer is over it.
        let drag_payload =
            egui::DragAndDrop::payload::<DragPayload>(ui.ctx()).map(|payload| (*payload).clone());
        let mut folder_drop_target: Option<(usize, ContainerId)> = None;
        let mut folder_drop_invalid = false;
        let mut pointer_over_card = false;
        let mut dragged = None;
        // Remember the right-click position on the canvas so new snippets,
        // folders, and texts can be created there. The field persists across
        // frames while the context menu stays open.
        if canvas_response.secondary_clicked() {
            canvas.menu_anchor = pointer_pos.map(|pos| {
                [
                    (pos.x - canvas_rect.min.x).max(0.0),
                    (pos.y - canvas_rect.min.y).max(0.0),
                ]
            });
        }

        let canvas_is_ai_box = data.workspace.is_ai_box(&canvas.container_id);
        let mut hovered_preview: Option<(PreviewTarget, egui::Rect)> = None;
        for index in 0..canvas.items.len() {
            let Some((title, kind)) = item_label(
                &canvas.container_id,
                &canvas.items[index],
                data.snippets,
                data.workspace,
                data.ai,
            ) else {
                continue;
            };
            let item = &canvas.items[index];
            let preview_target_for_hover = item.target.clone();
            let label = match kind {
                ItemKind::Folder => format!("📁 {title}"),
                ItemKind::Special => format!("⚙ {title}"),
                ItemKind::Snippet => title,
                // AI boxes are ordinary containers but stay visually distinct
                // from folders (a plain text prefix keeps it font-safe).
                ItemKind::AiBox => format!("AI {title}"),
                ItemKind::Conversation => title,
                ItemKind::ExternalFile => format!("📄 {title}"),
            };
            let galley = layout_title(
                painter,
                &label,
                CARD_WIDTH - 2.0 * CARD_PADDING_H,
                ui.visuals().text_color(),
            );
            // Persistent role tag inside AI boxes (`LINK · READ-ONLY` for
            // sources, `OUTPUT` for saved answers, `CONVERSATION` for chat
            // cards). The tag and the dashed/double outline keep the role
            // readable without hover, per plan_ai.md §4.3.
            let role = item.role;
            let tag_galley = if canvas_is_ai_box {
                let text = match role {
                    MemberRole::Source => "LINK · READ-ONLY",
                    MemberRole::Output => "OUTPUT",
                    MemberRole::Conversation => "CONVERSATION",
                    _ => "",
                };
                (!text.is_empty()).then(|| layout_tag(painter, text, ui.visuals()))
            } else {
                None
            };
            let tag_height = tag_galley
                .as_ref()
                .map_or(0.0, |tag| tag.size().y + 4.0);
            let card_size = egui::vec2(
                CARD_WIDTH,
                galley.size().y + 2.0 * CARD_MARGIN_Y + tag_height,
            );
            let rect = egui::Rect::from_min_size(
                canvas_rect.min + egui::vec2(item.position[0], item.position[1]),
                card_size,
            );
            canvas.items[index].size = card_size;
            pointer_over_card |= pointer_pos.is_some_and(|position| rect.contains(position));

            // Track hover for preview popup (not during drag operations).
            if let Some(pos) = pointer_pos
                && rect.contains(pos)
                && drag_payload.is_none()
                && canvas.dragging.is_none()
                && kind != ItemKind::Special
            {
                let preview_target = match &preview_target_for_hover {
                    ReferenceTarget::Snippet(id) => Some(PreviewTarget::Snippet(id.clone())),
                    ReferenceTarget::Container(id) => {
                        if data.workspace.is_ai_box(id) {
                            Some(PreviewTarget::AiBox(id.clone()))
                        } else {
                            Some(PreviewTarget::Folder(id.clone()))
                        }
                    }
                    ReferenceTarget::Conversation(id) => {
                        Some(PreviewTarget::Conversation(id.clone()))
                    }
                    ReferenceTarget::ExternalFile(file) => {
                        Some(PreviewTarget::ExternalFile(file.id.clone()))
                    }
                    ReferenceTarget::Special(_) => None,
                };
                if let Some(t) = preview_target {
                    hovered_preview = Some((t, rect));
                }
            }

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
            // A folder card under the pointer is a drop target for the dragged
            // card: the drop links/moves the reference into that folder.
            // The card must be the topmost widget under the pointer: while
            // dragging, the source canvas grows to follow the dragged card, so a
            // geometric check alone would also "hit" folder cards that are
            // actually covered by another window (full-window mode).
            if kind == ItemKind::Folder
                && let Some(payload) = &drag_payload
                && ui.ctx().rect_contains_pointer(ui.layer_id(), rect)
            {
                let folder_id = match &target {
                    ReferenceTarget::Container(id) => id.clone(),
                    _ => unreachable!("folder cards reference containers"),
                };
                let shift = ui.input(|input| input.modifiers.shift);
                if drop_valid_for(payload, &folder_id, data.snippets, data.workspace, shift) {
                    folder_drop_target = Some((index, folder_id));
                } else {
                    folder_drop_invalid = true;
                }
            }

            // Special (system) items have no context menu: they cannot be
            // linked, renamed, or deleted.
            let is_ai_box = data.workspace.is_ai_box(&canvas.container_id);
            let role = canvas.items[index].role;
            if kind != ItemKind::Special {
                response.context_menu(|ui| {
                    let origin = ui.ctx().viewport_id();
                    // Inside an AI box, member roles own the menu: read-only
                    // sources and conversation cards never expose Link / Move /
                    // Rename-entity / Delete-entity actions.
                    if is_ai_box && role == MemberRole::Source {
                        match &target {
                            ReferenceTarget::ExternalFile(file) => {
                                if ui.button("Open Externally").clicked() {
                                    commands.push(
                                        CanvasCommand::OpenExternalFile(file.clone()),
                                    );
                                    ui.close();
                                }
                            }
                            _ => {
                                if ui.button("Open Read-only").clicked() {
                                    match &target {
                                        ReferenceTarget::Snippet(id) => commands.push(
                                            CanvasCommand::OpenSnippetReadOnly(id.clone()),
                                        ),
                                        ReferenceTarget::Container(id) => {
                                            commands.push(
                                                CanvasCommand::OpenFolder(id.clone()),
                                            )
                                        }
                                        _ => {}
                                    }
                                    ui.close();
                                }
                                if let ReferenceTarget::Snippet(id) = &target
                                    && ui.button("Open Original").clicked()
                                {
                                    commands.push(
                                        CanvasCommand::OpenSnippet(id.clone()),
                                    );
                                    ui.close();
                                }
                            }
                        }
                        ui.separator();
                        if ui
                            .button("Remove Source")
                            .on_hover_text(
                                "Removing this card does not delete the original",
                            )
                            .clicked()
                        {
                            commands.push(CanvasCommand::RemoveAiSource {
                                ai_box: canvas.container_id.clone(),
                                reference: reference_id.clone(),
                            });
                            ui.close();
                        }
                        return;
                    }
                    if is_ai_box && role == MemberRole::Conversation {
                        if ui.button("Open").clicked() {
                            if let ReferenceTarget::Conversation(id) = &target {
                                commands.push(CanvasCommand::OpenConversation {
                                    ai_box: canvas.container_id.clone(),
                                    conversation: id.clone(),
                                });
                            }
                            ui.close();
                        }
                        if ui.button("Rename…").clicked() {
                            if let ReferenceTarget::Conversation(id) = &target {
                                commands.push(CanvasCommand::RenameConversation {
                                    ai_box: canvas.container_id.clone(),
                                    conversation: id.clone(),
                                    origin,
                                });
                            }
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Delete Conversation").clicked() {
                            if let ReferenceTarget::Conversation(id) = &target {
                                commands.push(CanvasCommand::DeleteConversation {
                                    ai_box: canvas.container_id.clone(),
                                    conversation: id.clone(),
                                });
                            }
                            ui.close();
                        }
                        return;
                    }
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
                        ReferenceTarget::Special(_) => {}
                        ReferenceTarget::Conversation(_) => {}
                        ReferenceTarget::ExternalFile(file) => {
                            if ui.button("Open Externally").clicked() {
                                commands.push(CanvasCommand::OpenExternalFile(file.clone()));
                                ui.close();
                            }
                            if ui.button("Rename…").clicked() {
                                commands.push(CanvasCommand::RenameExternalFile(file.id.clone()));
                                ui.close();
                            }
                        }
                    }
                    let last_link = reference_count(data.workspace, &target) == 1;
                    if ui
                        .button(if last_link { "Delete" } else { "Unlink" })
                        .clicked()
                    {
                        commands.push(CanvasCommand::DeleteReference {
                            owner: canvas.container_id.clone(),
                            reference: reference_id.clone(),
                            target: target.clone(),
                        });
                        ui.close();
                    }
                });
            }

            if response.clicked() {
                commands.push(match target {
                    ReferenceTarget::Snippet(id) if is_ai_box && role == MemberRole::Source => {
                        CanvasCommand::OpenSnippetReadOnly(id)
                    }
                    ReferenceTarget::Snippet(id) => CanvasCommand::OpenSnippet(id),
                    ReferenceTarget::Container(id) => CanvasCommand::OpenFolder(id),
                    ReferenceTarget::Special(kind) => CanvasCommand::OpenSpecial(kind),
                    ReferenceTarget::Conversation(id) => CanvasCommand::OpenConversation {
                        ai_box: canvas.container_id.clone(),
                        conversation: id,
                    },
                    // A click opens the external file with the system's default
                    // application (PDF viewer, Markdown editor, …).
                    ReferenceTarget::ExternalFile(file) => {
                        CanvasCommand::OpenExternalFile(file)
                    }
                });
            }
            // Every card (Special included) publishes a drag payload: the
            // cross-window drag-liveness signal used by `finalize_drops` is the
            // payload's presence, so a drag without one would bounce back every
            // frame and appear stuck. Special items stay unlinkable because
            // every drop/paste guard rejects `ReferenceTarget::Special`, so no
            // other canvas can ever accept their payload.
            if response.drag_started() {
                // Remember where the cursor grabbed the card so it stays glued
                // to the pointer while dragging (smooth in free and snap modes).
                let grab_offset = pointer_pos
                    .map(|pointer| pointer - rect.min)
                    .unwrap_or_default();
                canvas.dragging = Some(DragState {
                    index,
                    start_position: canvas.items[index].position,
                    invalid: false,
                    reference_id: reference_id.clone(),
                    grab_offset,
                });
                // Publish the drag to egui's shared payload so other canvases
                // (other windows) can act as drop targets.
                egui::DragAndDrop::set_payload(
                    ui.ctx(),
                    DragPayload::Reference {
                        source_container: canvas.container_id.clone(),
                        reference_id: reference_id.clone(),
                        target: canvas.items[index].target.clone(),
                    },
                );
            }
            if canvas
                .dragging
                .as_ref()
                .is_some_and(|drag| drag.index == index)
                && response.dragged()
            {
                dragged = Some(index);
                // Refresh the payload so it stays alive while dragging.
                egui::DragAndDrop::set_payload(
                    ui.ctx(),
                    DragPayload::Reference {
                        source_container: canvas.container_id.clone(),
                        reference_id: canvas.items[index].reference_id.clone(),
                        target: canvas.items[index].target.clone(),
                    },
                );
            }

            paint_card(
                painter,
                rect,
                &galley,
                canvas
                    .dragging
                    .as_ref()
                    .is_some_and(|drag| drag.index == index),
                kind,
                role,
                tag_galley.as_ref(),
                ui.visuals(),
            );
            // Highlight a folder card that is currently a drop target.
            if folder_drop_target
                .as_ref()
                .is_some_and(|(idx, _)| *idx == index)
            {
                painter.rect_stroke(
                    rect.expand(3.0),
                    4.0,
                    egui::Stroke::new(2.0, ui.visuals().selection.stroke.color),
                    egui::StrokeKind::Outside,
                );
            }
        }

        // ---- Hover preview ----
        {
            let preview = &mut *data.hover_preview;
            let now = Instant::now();
            let alt_held = ui.input(|i| i.modifiers.alt);

            if let Some((target, rect)) = &hovered_preview {
                // Pointer is over a previewable card in this canvas.
                if preview.target.as_ref() != Some(target) {
                    // New card: claim ownership.
                    *preview = HoverPreview {
                        target: Some(target.clone()),
                        canvas_id: Some(canvas.container_id.clone()),
                        hover_start: Some(now),
                        visible: alt_held,
                        anchor_rect: Some(*rect),
                        popup_rect: None,
                    };
                } else if preview.canvas_id == Some(canvas.container_id.clone()) {
                    // Same card, still owned by this canvas. Refresh the anchor
                    // rect so the popup tracks the card if the canvas scrolls.
                    preview.anchor_rect = Some(*rect);
                    if !preview.visible {
                        let timer_elapsed = preview.hover_start.is_some_and(|start| {
                            now.duration_since(start).as_millis() >= PREVIEW_DELAY_MS as u128
                        });
                        // Alt modifier gives an immediate preview, no timer.
                        preview.visible = alt_held || timer_elapsed;
                    }
                    // Popup position is derived from the anchor rect in
                    // render_float_preview, so no need to store it here.
                }
            } else if preview.canvas_id == Some(canvas.container_id.clone()) {
                // Pointer is not over any card in the owning canvas. Keep the
                // popup open only while the pointer is still over it.
                let ctx_pos = ui.ctx().input(|i| i.pointer.interact_pos());
                let over_popup = preview
                    .visible
                    && preview
                        .popup_rect
                        .zip(ctx_pos)
                        .is_some_and(|(popup_rect, pos)| popup_rect.contains(pos));
                if !over_popup {
                    // Pointer left both card and popup: dismiss.
                    *preview = HoverPreview::default();
                }
            }
            // Non-owning canvases: never touch the preview state.
        }

        // Render the hover preview popup (only in the owning canvas).
        if data.hover_preview.visible
            && data.hover_preview.canvas_id == Some(canvas.container_id.clone())
        {
            render_float_preview(ui.ctx(), data);
        }

        if let Some(index) = dragged {
            // Follow the pointer exactly (grab offset preserved), then snap the
            // target to the grid when enabled. Tracking the pointer instead of
            // accumulating per-frame deltas avoids the dead-zone/jump feel that
            // comes from snapping an already-snapped position every frame.
            if let Some(pointer) = pointer_pos {
                let grab_offset = canvas
                    .dragging
                    .as_ref()
                    .map(|drag| drag.grab_offset)
                    .unwrap_or_default();
                let mut target = [
                    pointer.x - grab_offset.x - canvas_rect.min.x,
                    pointer.y - grab_offset.y - canvas_rect.min.y,
                ];
                target[0] = target[0].max(0.0);
                target[1] = target[1].max(0.0);
                let item = &mut canvas.items[index];
                item.position = if data.snap_to_grid {
                    snap_position(target)
                } else {
                    target
                };
            }
            let rect = egui::Rect::from_min_size(
                egui::pos2(
                    canvas.items[index].position[0],
                    canvas.items[index].position[1],
                ),
                canvas.items[index].size,
            );
            // While hovering a valid folder drop target, that folder card is
            // excluded from the overlap check so the drop is not rejected.
            let drop_target = folder_drop_target.as_ref().map(|(idx, _)| *idx);
            let invalid = overlaps(
                canvas,
                Some(index),
                None,
                rect,
                data.snippets,
                data.workspace,
                data.ai,
                &text_layouts,
                drop_target,
            );
            if let Some(drag) = &mut canvas.dragging {
                drag.invalid = invalid;
            }
        }

        if canvas.dragging.as_ref().is_some_and(|drag| drag.invalid) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
        }
        if canvas.dragging.is_some() && !ui.input(|input| input.pointer.any_down()) {
            // Only a release whose pointer is actually over THIS canvas (its
            // window/layer is the topmost at the pointer) counts as a plain
            // reposition. While dragging, the source canvas rect grows to follow
            // the dragged card, so the pointer can be geometrically inside it
            // while really hovering another window (full-window mode); such a
            // release belongs to the drop targets and is finalized by
            // `HomePage::finalize_drops` instead.
            let pointer_really_over_canvas = pointer_pos
                .is_some_and(|_| ui.ctx().rect_contains_pointer(ui.layer_id(), canvas_rect));
            let dropped_on_folder = pointer_really_over_canvas
                && folder_drop_target.as_ref().is_some_and(|(idx, _)| {
                    canvas
                        .dragging
                        .as_ref()
                        .is_some_and(|drag| *idx != drag.index)
                });
            if pointer_really_over_canvas && !dropped_on_folder {
                // Plain reposition within this canvas.
                let drag = canvas.dragging.take().expect("drag state disappeared");
                if drag.invalid {
                    canvas.items[drag.index].position = drag.start_position;
                } else {
                    canvas.save_layout(data.workspace_store);
                }
                egui::DragAndDrop::clear_payload(ui.ctx());
            } else if dropped_on_folder {
                // Dropped on a folder card in this canvas: the drop branch
                // below emits the DropOnCanvas command; the source card
                // returns home.
                let drag = canvas.dragging.take().expect("drag state disappeared");
                canvas.items[drag.index].position = drag.start_position;
                canvas.save_layout(data.workspace_store);
            }
            // Released over a different window: keep the `DragState`; the
            // frame-end `HomePage::finalize_drops` bounces the card back unless
            // a drop target consumed the payload.
        }

        // ---- Drop targets (payload-based, cross-window) ----
        if let Some(payload) = &drag_payload {
            let DragPayload::Reference {
                source_container, ..
            } = payload;
            let move_semantics = ui.input(|input| input.modifiers.shift);
            // Layer-aware hover: the canvas must be the topmost window under the
            // pointer to be a drop target (see the source-canvas release above).
            let hovering_canvas = ui.ctx().rect_contains_pointer(ui.layer_id(), canvas_rect);
            let hovering_folder_card = folder_drop_target.is_some() || folder_drop_invalid;
            let canvas_drop_valid = source_container != &canvas.container_id
                && hovering_canvas
                && !hovering_folder_card
                && drop_valid_for(
                    payload,
                    &canvas.container_id,
                    data.snippets,
                    data.workspace,
                    move_semantics,
                );

            // Cursor semantics.
            if folder_drop_invalid {
                ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
            } else if folder_drop_target.is_some() {
                ui.ctx().set_cursor_icon(if move_semantics {
                    egui::CursorIcon::Move
                } else {
                    egui::CursorIcon::Copy
                });
            } else if source_container != &canvas.container_id && hovering_canvas {
                if !drop_valid_for(
                    payload,
                    &canvas.container_id,
                    data.snippets,
                    data.workspace,
                    move_semantics,
                ) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::NotAllowed);
                } else if move_semantics {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                } else {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Copy);
                }
            }

            // Drop on a folder card: link/move the reference into that folder.
            if let Some((_, folder_id)) = &folder_drop_target
                && ui.input(|input| input.pointer.primary_released())
            {
                commands.push(CanvasCommand::DropOnCanvas {
                    container: folder_id.clone(),
                    position: None,
                    payload: payload.clone(),
                    move_semantics,
                });
                egui::DragAndDrop::clear_payload(ui.ctx());
            }
            // Drop on the empty area of a *different* canvas.
            else if canvas_drop_valid && ui.input(|input| input.pointer.primary_released()) {
                let position = pointer_pos.map(|pointer| {
                    let position = [
                        (pointer.x - canvas_rect.min.x).max(0.0),
                        (pointer.y - canvas_rect.min.y).max(0.0),
                    ];
                    if data.snap_to_grid {
                        snap_position(position)
                    } else {
                        position
                    }
                });
                commands.push(CanvasCommand::DropOnCanvas {
                    container: canvas.container_id.clone(),
                    position,
                    payload: payload.clone(),
                    move_semantics,
                });
                egui::DragAndDrop::clear_payload(ui.ctx());
            }

            // Visual feedback: highlight the canvas and preview the drop.
            if canvas_drop_valid && let Some(pointer) = pointer_pos {
                paint_drop_preview(painter, canvas_rect, pointer, move_semantics, ui.visuals());
            }
        }

        let pointer_over_text =
            render_canvas_texts(ui, canvas, data, canvas_rect, &text_layouts, pointer_pos);

        // OS file drop: one external-file card per dropped file (files without
        // a path, e.g. pasted content, are skipped). The frame-global
        // `dropped_files` is consumed by exactly one canvas (the per-frame
        // guard), attributed geometrically when the pointer is known; in native
        // mode each viewport has a single canvas so the event can never leak
        // into another window.
        if canvas_accepts_files
            && !dropped_files.is_empty()
            && !*data.os_file_drop_consumed
            && (pointer_over_canvas || !data.floating || file_pointer.is_none())
        {
            *data.os_file_drop_consumed = true;
            let mut position = file_drop_pos;
            for file in dropped_files {
                let Some(path) = file.path else {
                    continue;
                };
                let path = path.display().to_string();
                log::info!("OS file drop -> inserting external file card: {path}");
                create_external_file(canvas, data, path, position);
                // Cascade additional files below the previous one.
                position = position.map(|[x, y]| [x, y + 36.0]);
            }
        }

        if !pointer_over_card && !pointer_over_text {
            canvas_response.context_menu(|ui| {
                if ui.button("Search…").clicked() {
                    commands.push(CanvasCommand::OpenSearch);
                    ui.close();
                }
                ui.separator();
                let ai_box = canvas.container_id.clone();
                let anchor = canvas.menu_anchor;
                if data.workspace.is_ai_box(&canvas.container_id) {
                    // Inside an AI box: create a conversation or link a source.
                    if ui.button("New Conversation…").clicked() {
                        commands.push(CanvasCommand::NewConversation {
                            ai_box: ai_box.clone(),
                            position: anchor,
                        });
                        ui.close();
                    }
                    ui.menu_button("Link Source…", |ui| {
                        ui.label(egui::RichText::new("Notes").small().weak());
                        for (id, snippet) in data.snippets.iter() {
                            if ui.button(&snippet.title).clicked() {
                                commands.push(CanvasCommand::LinkAiSource {
                                    ai_box: ai_box.clone(),
                                    target: ReferenceTarget::Snippet(id.clone()),
                                    position: anchor,
                                });
                                ui.close();
                            }
                        }
                        ui.separator();
                        ui.label(egui::RichText::new("Folders").small().weak());
                        // Other folders (and AI boxes) can be linked as a
                        // container source; the AI box itself is excluded to
                        // avoid self-reference.
                        let self_id = canvas.container_id.clone();
                        for (id, container) in data
                            .workspace
                            .containers
                            .iter()
                            .filter(|(id, _)| **id != self_id)
                        {
                            if ui.button(&container.title).clicked() {
                                commands.push(CanvasCommand::LinkAiSource {
                                    ai_box: ai_box.clone(),
                                    target: ReferenceTarget::Container(id.clone()),
                                    position: anchor,
                                });
                                ui.close();
                            }
                        }
                        // Collect unique external file references from the
                        // workspace as linkable sources.
                        let mut seen = BTreeSet::new();
                        let external_files: Vec<ExternalFileRef> = data
                            .workspace
                            .containers
                            .values()
                            .flat_map(|container| &container.members)
                            .filter_map(|reference| match &reference.target {
                                ReferenceTarget::ExternalFile(file) => {
                                    if seen.insert(file.id.clone()) {
                                        Some(file.clone())
                                    } else {
                                        None
                                    }
                                }
                                _ => None,
                            })
                            .collect();
                        if !external_files.is_empty() {
                            ui.separator();
                            ui.label(egui::RichText::new("External Files").small().weak());
                            for file in external_files {
                                if ui.button(&file.title).clicked() {
                                    commands.push(CanvasCommand::LinkAiSource {
                                        ai_box: ai_box.clone(),
                                        target: ReferenceTarget::ExternalFile(file),
                                        position: anchor,
                                    });
                                    ui.close();
                                }
                            }
                        }
                    });
                    ui.separator();
                } else if ui.button("New AI Box…").clicked() {
                    commands.push(CanvasCommand::NewAiBox {
                        owner: canvas.container_id.clone(),
                        position: anchor,
                    });
                    ui.close();
                }
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
                        ReferenceTarget::Special(special) => special.label(),
                        // Conversation cards cannot be picked up to the
                        // clipboard; this arm is unreachable.
                        ReferenceTarget::Conversation(_) => "Conversation",
                        ReferenceTarget::ExternalFile(file) => file.title.as_str(),
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
                    let anchor = canvas.menu_anchor;
                    create_snippet(canvas, data, anchor);
                    ui.close();
                }
                if ui.button("New Folder").clicked() {
                    let anchor = canvas.menu_anchor;
                    create_folder(canvas, data, anchor);
                    ui.close();
                }
                if ui.button("Insert External File…").clicked() {
                    commands.push(CanvasCommand::OpenFileInsertDialog(
                        canvas.container_id.clone(),
                    ));
                    ui.close();
                }
                if ui.button("New Text").clicked() {
                    let anchor = canvas.menu_anchor;
                    create_text(
                        canvas,
                        data,
                        anchor.unwrap_or_else(|| default_text_position(canvas)),
                    );
                    ui.close();
                }
                if ui.button("Organize").clicked() {
                    // Follows the snap-to-grid setting: on → dense packing
                    // without gaps, off → grid-aligned with breathing room.
                    canvas.organize(data.workspace_store, available.y, data.snap_to_grid);
                    ui.close();
                }
            });
        }
    }
}

/// Scale factor applied to the mini-canvas shown inside a folder/AI-box hover
/// preview: card sizes, positions, and margins are all multiplied by this.
const PREVIEW_SCALE: f32 = 0.8;

/// Items inside a folder or AI box, rendered as mini cards in the preview.
pub(super) struct PreviewCard {
    pub(super) label: String,
    pub(super) kind: ItemKind,
    pub(super) role: MemberRole,
    /// Position from the container layout (canvas coordinates).
    pub(super) position: [f32; 2],
    /// Canvas-space height of the card. When scaled by [`PREVIEW_SCALE`] it
    /// exactly fits the title/role-tag galleys the preview renders.
    pub(super) height: f32,
}

/// Measures the rendered height of a text laid out with `font_id` at
/// `max_width` (without a painter; used to size mini preview cards the same
/// way the real canvas sizes them).
fn text_height(ctx: &egui::Context, text: &str, font_id: egui::FontId, max_width: f32) -> f32 {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id,
            color: egui::Color32::WHITE,
            ..Default::default()
        },
    );
    job.wrap.max_width = max_width.max(10.0);
    let painter = ctx.layer_painter(egui::LayerId::debug());
    painter.layout_job(job).size().y
}

/// Builds the mini cards shown in a folder/AI-box hover preview, mirroring
/// exactly how the real canvas resolves members: unresolved targets are
/// dropped, and members without a saved layout position fall back to the same
/// default grid slot the real canvas would use.
pub(super) fn container_preview_cards(
    ctx: &egui::Context,
    container: &Container,
    layout: &ContainerLayout,
    data: &CanvasData<'_>,
) -> (Vec<PreviewCard>, usize) {
    let members: Vec<&Reference> = container
        .members
        .iter()
        .filter(|r| match &r.target {
            ReferenceTarget::Snippet(id) => data.snippets.contains_key(id),
            ReferenceTarget::Container(id) => data.workspace.containers.contains_key(id),
            ReferenceTarget::Special(_)
            | ReferenceTarget::Conversation(_)
            | ReferenceTarget::ExternalFile(_) => true,
        })
        .collect();
    let cards: Vec<PreviewCard> = members
        .iter()
        .enumerate()
        .filter_map(|(index, r)| {
            let item = CanvasItem {
                reference_id: r.id.clone(),
                target: r.target.clone(),
                role: r.role,
                position: [0.0, 0.0],
                size: egui::vec2(CARD_WIDTH, 25.0),
            };
            let (label, kind) =
                item_label(&container.id, &item, data.snippets, data.workspace, data.ai)?;
            let role = r.role;
            let tag_text = match role {
                MemberRole::Source => "LINK · READ-ONLY",
                MemberRole::Output => "OUTPUT",
                MemberRole::Conversation => "CONVERSATION",
                _ => "",
            };
            // Measure at the same (scaled) width the preview uses to lay out
            // the galleys, so `height * PREVIEW_SCALE` exactly fits them.
            let title_w = (CARD_WIDTH - 2.0 * CARD_PADDING_H) * PREVIEW_SCALE;
            let title_height = text_height(
                ctx,
                &label,
                egui::FontId::proportional(14.0),
                title_w,
            );
            let tag_height = if tag_text.is_empty() {
                0.0
            } else {
                text_height(
                    ctx,
                    tag_text,
                    egui::FontId::proportional(9.0),
                    title_w,
                ) + 2.0
            };
            let position = layout
                .items
                .get(&r.id)
                .map(|l| l.position)
                .unwrap_or_else(|| default_card_position(index));
            Some(PreviewCard {
                label,
                kind,
                role,
                position,
                height: (CARD_MARGIN_Y + title_height + tag_height + 2.0) / PREVIEW_SCALE,
            })
        })
        .collect();
    let total = cards.len();
    (cards, total)
}

/// Renders the hover-preview popup for a canvas card. Called after the card
/// loop when the timer has expired and the pointer is still over the card.
fn render_float_preview(ctx: &egui::Context, data: &mut CanvasData<'_>) {
    let Some(target) = &data.hover_preview.target else {
        return;
    };

    // Popup anchor: use the hovered card's rect (viewport coordinates) so the
    // popup stays next to the card while the pointer moves inside it, instead
    // of sliding around following the cursor. Falls back to the pointer only
    // if no card rect was captured.
    let popup_pos = data
        .hover_preview
        .anchor_rect
        .map(|anchor| egui::pos2(anchor.right() + 8.0, anchor.top()))
        .or_else(|| ctx.input(|i| i.pointer.interact_pos()).map(|p| p + egui::vec2(12.0, -8.0)))
        .unwrap_or(egui::pos2(100.0, 100.0));

    enum PreviewBody {
        Text(String),
        /// Mini cards rendered at their actual canvas positions (offset by
        /// the bounding-box origin so they fit in the popup).
        Cards {
            cards: Vec<PreviewCard>,
            bbox_min: egui::Vec2,
            bbox_max: egui::Vec2,
            total: usize,
        },
    }
    struct PreviewData {
        body: PreviewBody,
    }

    let preview: Option<PreviewData> = match target {
        PreviewTarget::Snippet(id) => data.snippets.get(id).map(|s| {
            let lines: Vec<&str> = s.content.lines().take(30).collect();
            PreviewData {
                body: PreviewBody::Text(lines.join("\n")),
            }
        }),
        PreviewTarget::Folder(id) | PreviewTarget::AiBox(id) => {
            let layout = data
                .workspace_store
                .load_layout(id)
                .unwrap_or_else(|_| ContainerLayout::empty(id.clone()));
            data.workspace.containers.get(id).map(|c| {
                let (cards, total) = container_preview_cards(ctx, c, &layout, data);
                let bbox_min = cards.iter().fold(egui::Vec2::splat(f32::MAX), |m, c| {
                    egui::vec2(m.x.min(c.position[0]), m.y.min(c.position[1]))
                });
                let bbox_max = cards.iter().fold(egui::Vec2::splat(f32::MIN), |m, c| {
                    egui::vec2(
                        m.x.max(c.position[0] + CARD_WIDTH),
                        m.y.max(c.position[1] + c.height),
                    )
                });
                PreviewData {
                    body: PreviewBody::Cards {
                        cards,
                        bbox_min,
                        bbox_max,
                        total,
                    },
                }
            })
        }
        PreviewTarget::Conversation(id) => {
            data.ai.values().find_map(|box_data| {
                box_data.get(id).map(|conv| {
                    let source_count = conv.sources.len();
                    PreviewData {
                        body: PreviewBody::Text(format!(
                            "{source_count} sources · {} messages",
                            conv.messages.len()
                        )),
                    }
                })
            })
        }
        PreviewTarget::ExternalFile(id) => {
            let mut found: Option<PreviewData> = None;
            for container in data.workspace.containers.values() {
                for r in &container.members {
                    if let ReferenceTarget::ExternalFile(file) = &r.target
                        && file.id == *id
                    {
                        found = Some(PreviewData {
                            body: PreviewBody::Text(file.path.clone()),
                        });
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            found
        }
    };

    let Some(preview) = preview else {
        data.hover_preview.target = None;
        data.hover_preview.visible = false;
        return;
    };

    let screen = ctx.input(|i| i.raw.screen_rect).unwrap_or(egui::Rect::from_min_max(
        egui::pos2(0.0, 0.0),
        egui::pos2(1920.0, 1080.0),
    ));

    let popup_size = match &preview.body {
        PreviewBody::Cards { bbox_min, bbox_max, .. } => {
            let w = ((bbox_max.x - bbox_min.x) * 0.8 + 60.0).clamp(200.0, 440.0);
            let h = ((bbox_max.y - bbox_min.y) * 0.8 + 80.0).clamp(80.0, 320.0);
            egui::vec2(w, h)
        }
        PreviewBody::Text(_) => egui::vec2(360.0, 240.0),
    };

    let clamped_pos = egui::pos2(
        popup_pos
            .x
            .clamp(screen.left() + 8.0, screen.right() - popup_size.x - 8.0),
        popup_pos
            .y
            .clamp(screen.top() + 8.0, screen.bottom() - popup_size.y - 8.0),
    );

    let area_id = egui::Id::new("float-preview");
    let area = egui::Area::new(area_id)
        .order(egui::Order::Foreground)
        .fixed_pos(clamped_pos);

    let response = area.show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| {
            ui.set_min_size(popup_size);
            ui.set_max_size(popup_size);
            match &preview.body {
                PreviewBody::Text(content) => {
                    egui::ScrollArea::vertical()
                        .id_salt("float-preview-scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.label(content.as_str());
                        });
                }
                PreviewBody::Cards {
                    cards,
                    bbox_min,
                    bbox_max,
                    total,
                } => {
                    if cards.is_empty() {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label("(empty)");
                        });
                    } else {
                        let card_w = CARD_WIDTH * PREVIEW_SCALE;
                        let margin = 16.0 * PREVIEW_SCALE;
                        let canvas_w = ((bbox_max.x - bbox_min.x) * PREVIEW_SCALE + margin * 2.0)
                            .max(60.0);
                        let canvas_h = ((bbox_max.y - bbox_min.y) * PREVIEW_SCALE + margin * 2.0)
                            .max(40.0);
                        egui::ScrollArea::both()
                            .id_salt("float-preview-canvas")
                            .auto_shrink([false, false])
                            .max_height(popup_size.y - 8.0)
                            .show(ui, |ui| {
                                let (response, painter) = ui.allocate_painter(
                                    egui::vec2(canvas_w, canvas_h),
                                    egui::Sense::hover(),
                                );
                                // Painter shapes are in absolute screen
                                // coordinates clipped to the allocated rect, so
                                // the canvas-local card positions must be
                                // offset by the mini-canvas origin.
                                let origin = response.rect.min;
                                let painter = &painter;
                                for card in cards {
                                    let x = origin.x
                                        + (card.position[0] - bbox_min.x) * PREVIEW_SCALE
                                        + margin;
                                    let y = origin.y
                                        + (card.position[1] - bbox_min.y) * PREVIEW_SCALE
                                        + margin;
                                    let rect = egui::Rect::from_min_size(
                                        egui::pos2(x, y),
                                        egui::vec2(card_w, card.height * PREVIEW_SCALE),
                                    );
                                    let galley = layout_title(
                                        painter,
                                        &card.label,
                                        card_w - 2.0 * CARD_PADDING_H * PREVIEW_SCALE,
                                        ui.visuals().text_color(),
                                    );
                                    let tag_galley = match card.role {
                                        MemberRole::Source
                                        | MemberRole::Output
                                        | MemberRole::Conversation => {
                                            let text = match card.role {
                                                MemberRole::Source => "LINK · READ-ONLY",
                                                MemberRole::Output => "OUTPUT",
                                                MemberRole::Conversation => "CONVERSATION",
                                                _ => "",
                                            };
                                            Some(layout_tag(painter, text, ui.visuals()))
                                        }
                                        _ => None,
                                    };
                                    paint_card(
                                        painter,
                                        rect,
                                        &galley,
                                        false,
                                        card.kind,
                                        card.role,
                                        tag_galley.as_ref(),
                                        ui.visuals(),
                                    );
                                }
                            });
                        if *total > cards.len() {
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(format!("… and {} more", total - cards.len()))
                                    .small()
                                    .weak(),
                            );
                        }
                    }
                }
            }
        });
    });

    data.hover_preview.popup_rect = Some(response.response.rect);
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
            egui::Id::new((
                "canvas-text",
                canvas.container_id.as_str(),
                text_id.as_str(),
            )),
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
            canvas.texts[index].position[0] = (canvas.texts[index].position[0] + delta.x).max(0.0);
            canvas.texts[index].position[1] = (canvas.texts[index].position[1] + delta.y).max(0.0);
            let drag_rect = egui::Rect::from_min_size(
                egui::pos2(
                    canvas.texts[index].position[0],
                    canvas.texts[index].position[1],
                ),
                layout.rect.size(),
            );
            let invalid = overlaps(
                canvas,
                None,
                Some(index),
                drag_rect,
                data.snippets,
                data.workspace,
                data.ai,
                text_layouts,
                None,
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

/// The visual kind of a canvas card, used to pick the label prefix, card
/// styling, and interaction rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ItemKind {
    Snippet,
    Folder,
    Special,
    /// An AI workspace container (distinct from a plain folder).
    AiBox,
    /// An AI conversation card inside an AI box.
    Conversation,
    /// An inserted external file (PDF, Markdown, …), opened externally on click.
    ExternalFile,
}

pub(super) fn item_label(
    container: &ContainerId,
    item: &CanvasItem,
    snippets: &BTreeMap<EntityId, Snippet>,
    workspace: &Workspace,
    ai: &BTreeMap<ContainerId, AiBoxData>,
) -> Option<(String, ItemKind)> {
    let (title, kind) = match &item.target {
        ReferenceTarget::Snippet(id) => (snippets.get(id)?.title.as_str(), ItemKind::Snippet),
        ReferenceTarget::Container(id) => {
            let container = workspace.containers.get(id)?;
            if container.kind == ContainerKind::AiWorkspace {
                (container.title.as_str(), ItemKind::AiBox)
            } else {
                (container.title.as_str(), ItemKind::Folder)
            }
        }
        ReferenceTarget::Special(special) => (special.label(), ItemKind::Special),
        // External file cards are self-contained: the title travels with the
        // reference (renamed via the card's context menu).
        ReferenceTarget::ExternalFile(file) => (file.title.as_str(), ItemKind::ExternalFile),
        // The conversation title is resolved from the AI sidecar store when the
        // canvas is rendered inside an AI box; the placeholder keeps the card
        // renderable even if the sidecar entry is missing.
        ReferenceTarget::Conversation(id) => {
            let title = ai
                .get(container)
                .and_then(|data| data.get(id))
                .map(|conversation| conversation.title.as_str())
                .unwrap_or("Conversation");
            (title, ItemKind::Conversation)
        }
    };
    (!title.is_empty()).then(|| (title.to_owned(), kind))
}

/// Creates an external-file card from an absolute `path` at `position` (or a
/// free default slot when `None`), mirroring `create_snippet`: the reference is
/// persisted to the workspace and the card to the container layout. The file on
/// disk is never touched. Used by the OS-level file drop and shared with any
/// future direct-insert path.
fn create_external_file(
    canvas: &mut ContainerCanvas,
    data: &mut CanvasData<'_>,
    path: String,
    position: Option<[f32; 2]>,
) {
    let position = position.unwrap_or_else(|| canvas.default_position(data));
    let file = ExternalFileRef {
        id: ExternalFileId::new(),
        title: file_stem(&path),
        path,
    };
    let is_ai_box = data.workspace.is_ai_box(&canvas.container_id);
    let result = if is_ai_box {
        data.workspace.add_source_reference(&canvas.container_id, ReferenceTarget::ExternalFile(file.clone()))
    } else {
        data.workspace.add_external_file_reference(&canvas.container_id, file.clone())
    };
    let Ok(reference_id) = result else {
        return;
    };
    let _ = data.workspace_store.save(data.workspace);
    let role = if is_ai_box { MemberRole::Source } else { MemberRole::Normal };
    canvas.items.push(CanvasItem {
        reference_id: reference_id.clone(),
        target: ReferenceTarget::ExternalFile(file),
        role,
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

fn create_snippet(
    canvas: &mut ContainerCanvas,
    data: &mut CanvasData<'_>,
    position: Option<[f32; 2]>,
) {
    // Duplicate titles are fine; identity is the ulid.
    let title = "Untitled".to_owned();
    let position = position.unwrap_or_else(|| canvas.default_position(data));
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
        role: MemberRole::Normal,
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

fn create_folder(
    canvas: &mut ContainerCanvas,
    data: &mut CanvasData<'_>,
    position: Option<[f32; 2]>,
) {
    // Duplicate titles are fine; identity is the ulid.
    let title = "New Folder".to_owned();
    let position = position.unwrap_or_else(|| canvas.default_position(data));
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
        role: MemberRole::Normal,
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
    container: &ContainerId,
    items: &[CanvasItem],
    snippets: &BTreeMap<EntityId, Snippet>,
    workspace: &Workspace,
    ai: &BTreeMap<ContainerId, AiBoxData>,
    text_rects: &[egui::Rect],
) -> [f32; 2] {
    for index in 0..640 {
        let position = default_card_position(index);
        let candidate = egui::Rect::from_min_size(
            egui::pos2(position[0], position[1]),
            egui::vec2(CARD_WIDTH, 40.0),
        );
        let occupied = items.iter().any(|item| {
            item_label(container, item, snippets, workspace, ai).is_some()
                && egui::Rect::from_min_size(
                    egui::pos2(item.position[0], item.position[1]),
                    item.size,
                )
                .intersects(candidate)
        }) || text_rects
            .iter()
            .any(|text_rect| text_rect.intersects(candidate));
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
        // Special items cannot be linked or pasted.
        ReferenceTarget::Special(_) => false,
        // Conversation cards cannot be linked or pasted.
        ReferenceTarget::Conversation(_) => false,
        // External files can now be linked into AI boxes as model-readable
        // sources; their content is extracted at conversation time.
        ReferenceTarget::ExternalFile(_) => true,
    }
}

/// Whether dropping `payload` into `container` is currently allowed (the same
/// rules as [`clipboard_valid_for`], with the source container checked for
/// moves). Drop targets reject invalid drops with a `NotAllowed` cursor.
pub(super) fn drop_valid_for(
    payload: &DragPayload,
    container: &ContainerId,
    snippets: &BTreeMap<EntityId, Snippet>,
    workspace: &Workspace,
    move_semantics: bool,
) -> bool {
    let DragPayload::Reference {
        source_container,
        target,
        ..
    } = payload;
    if move_semantics && source_container == container {
        return false;
    }
    match target {
        ReferenceTarget::Snippet(entity_id) => snippets.contains_key(entity_id),
        ReferenceTarget::Container(target_id) => {
            workspace.containers.contains_key(target_id) && target_id != container
        }
        // Special items cannot be linked or dropped into other containers.
        ReferenceTarget::Special(_) => false,
        // Conversation cards cannot be linked or dropped into other containers.
        ReferenceTarget::Conversation(_) => false,
        // External files can now be linked into AI boxes as model-readable
        // sources; their content is extracted at conversation time.
        ReferenceTarget::ExternalFile(_) => true,
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
    // In floating-window mode every canvas shares the root viewport, so the
    // status is only shown on the root canvas to avoid duplicating it in every
    // floating window.
    if data.floating && !data.is_root {
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
        ReferenceTarget::ExternalFile(file) => file.title.clone(),
        ReferenceTarget::Special(special) => special.label().to_owned(),
        // Conversation cards cannot be picked up to the clipboard; unreachable.
        ReferenceTarget::Conversation(_) => "Conversation".to_owned(),
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

/// Paints a dot grid: minor dots every cell and slightly larger, stronger dots
/// every fifth cell, so the canvas reads as a structured work surface. The
/// base `color` is used for major intersections; minor dots are derived from it.
fn paint_grid(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32) {
    const STEP: f32 = 32.0;
    const MAJOR_EVERY: i32 = 5;
    let minor_color = color.gamma_multiply(0.65);
    let (mut row, mut y) = (1, rect.min.y + STEP);
    while y < rect.max.y {
        let (mut col, mut x) = (1, rect.min.x + STEP);
        while x < rect.max.x {
            let major = col % MAJOR_EVERY == 0 && row % MAJOR_EVERY == 0;
            let (radius, dot_color) = if major {
                (1.8, color)
            } else {
                (1.1, minor_color)
            };
            painter.circle_filled(egui::pos2(x, y), radius, dot_color);
            col += 1;
            x += STEP;
        }
        row += 1;
        y += STEP;
    }
}

/// Rounds a canvas position to the nearest 32 pt grid point.
pub(super) fn snap_position(position: [f32; 2]) -> [f32; 2] {
    const STEP: f32 = 32.0;
    [
        (position[0] / STEP).round() * STEP,
        (position[1] / STEP).round() * STEP,
    ]
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

/// Paints the cross-window drop indicator: a highlighted canvas border plus a
/// preview card under the pointer, labeled with the drop semantics.
fn paint_drop_preview(
    painter: &egui::Painter,
    canvas_rect: egui::Rect,
    pointer: egui::Pos2,
    move_semantics: bool,
    visuals: &egui::Visuals,
) {
    let accent = visuals.selection.stroke.color;
    painter.rect_stroke(
        canvas_rect.expand(2.0),
        4.0,
        egui::Stroke::new(2.0, accent.gamma_multiply(0.8)),
        egui::StrokeKind::Outside,
    );
    let preview = egui::Rect::from_center_size(pointer, egui::vec2(CARD_WIDTH, 34.0));
    painter.rect_stroke(
        preview,
        2.5,
        egui::Stroke::new(1.5, accent),
        egui::StrokeKind::Inside,
    );
    let label = if move_semantics { "Move" } else { "Link" };
    let galley = layout_title(painter, label, 60.0, accent);
    let label_rect = egui::Rect::from_min_size(preview.max + egui::vec2(6.0, 4.0), galley.size());
    painter.rect_filled(label_rect.expand(3.0), 3.0, visuals.panel_fill);
    painter.galley(label_rect.min, galley, accent);
}

#[allow(clippy::too_many_arguments)]
fn paint_card(
    painter: &egui::Painter,
    rect: egui::Rect,
    galley: &std::sync::Arc<egui::Galley>,
    dragging: bool,
    kind: ItemKind,
    role: MemberRole,
    tag: Option<&std::sync::Arc<egui::Galley>>,
    visuals: &egui::Visuals,
) {
    let source_card = role == MemberRole::Source;
    let conversation_card = role == MemberRole::Conversation;
    let bg = if dragging {
        visuals.widgets.active.bg_fill
    } else if kind == ItemKind::Special {
        visuals.widgets.hovered.bg_fill.gamma_multiply(0.5)
    } else if kind == ItemKind::Folder {
        visuals.widgets.hovered.bg_fill.gamma_multiply(0.7)
    } else if kind == ItemKind::AiBox {
        visuals.widgets.hovered.bg_fill.gamma_multiply(0.8)
    } else if kind == ItemKind::ExternalFile {
        // A "reference out" tint: lighter than an ordinary card, hinting that
        // the object lives outside the workspace.
        visuals.widgets.hovered.bg_fill.gamma_multiply(0.6)
    } else if source_card {
        // Lighter fill than an ordinary card: a read-only source is context,
        // not an owned object.
        visuals.widgets.inactive.bg_fill.gamma_multiply(0.55)
    } else {
        visuals.widgets.inactive.bg_fill
    };
    let stroke = if dragging {
        egui::Stroke::new(2.0, visuals.selection.stroke.color)
    } else if kind == ItemKind::Special {
        egui::Stroke::new(1.5, visuals.selection.stroke.color.gamma_multiply(0.5))
    } else if kind == ItemKind::Folder {
        egui::Stroke::new(1.5, visuals.selection.stroke.color.gamma_multiply(0.65))
    } else if kind == ItemKind::AiBox {
        egui::Stroke::new(1.5, visuals.selection.stroke.color.gamma_multiply(0.8))
    } else if kind == ItemKind::ExternalFile {
        egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.9))
    } else if source_card {
        egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.8))
    } else {
        egui::Stroke::new(1.0, visuals.widgets.inactive.bg_stroke.color)
    };
    painter.rect(rect, 2.5, bg, stroke, egui::StrokeKind::Inside);
    if source_card && !dragging {
        // Persistent dashed outline: read-only sources are never solid-bordered.
        let corners = [
            rect.left_top(),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom(),
            rect.left_top(),
        ];
        painter.add(egui::Shape::dashed_line(
            &corners,
            egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.9)),
            4.0,
            3.0,
        ));
    } else if conversation_card && !dragging {
        // Double outline: a conversation card is a distinct two-part border.
        painter.rect_stroke(
            rect.shrink(1.5),
            2.0,
            egui::Stroke::new(1.0, visuals.weak_text_color().gamma_multiply(0.7)),
            egui::StrokeKind::Inside,
        );
    }

    painter.with_clip_rect(rect).galley(
        rect.min + egui::vec2(CARD_PADDING_H, CARD_MARGIN_Y),
        galley.clone(),
        visuals.text_color(),
    );
    if let Some(tag) = tag {
        let tag_pos = rect.min + egui::vec2(CARD_PADDING_H, CARD_MARGIN_Y + galley.size().y + 2.0);
        let tag_color = if source_card {
            visuals.weak_text_color().gamma_multiply(0.9)
        } else {
            visuals.selection.stroke.color.gamma_multiply(0.8)
        };
        painter.with_clip_rect(rect).galley(tag_pos, tag.clone(), tag_color);
    }
}

/// Lays out the small persistent role tag inside an AI box card.
fn layout_tag(
    painter: &egui::Painter,
    text: &str,
    visuals: &egui::Visuals,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        text,
        0.0,
        egui::TextFormat {
            font_id: egui::FontId::proportional(9.0),
            color: visuals.weak_text_color(),
            ..Default::default()
        },
    );
    job.wrap.max_width = (CARD_WIDTH - 2.0 * CARD_PADDING_H).max(10.0);
    painter.layout_job(job)
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
/// itself (`card_index` / `text_index`) and an optional additional card
/// (`exclude_card`, used to allow dropping onto a folder drop target).
#[allow(clippy::too_many_arguments)]
fn overlaps(
    canvas: &ContainerCanvas,
    card_index: Option<usize>,
    text_index: Option<usize>,
    rect: egui::Rect,
    snippets: &BTreeMap<EntityId, Snippet>,
    workspace: &Workspace,
    ai: &BTreeMap<ContainerId, AiBoxData>,
    text_layouts: &[TextLayout],
    exclude_card: Option<usize>,
) -> bool {
    let card_overlap = canvas.items.iter().enumerate().any(|(index, item)| {
        Some(index) != card_index
            && Some(index) != exclude_card
            && item_label(&canvas.container_id, item, snippets, workspace, ai).is_some()
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
            egui::Rect::from_min_size(egui::pos2(item.position[0], item.position[1]), item.size)
                .intersects(candidate)
        }) || text_rects.iter().any(|rect| rect.intersects(candidate));
        if !occupied {
            return position;
        }
    }
    [24.0, 24.0]
}
