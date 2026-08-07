use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use eframe::{egui, App};

use floatdea::data::{
    storage::SnippetStore,
    workspace::{CanvasText, CardLayout, ContainerLayout, ReferenceTarget, Workspace, WorkspaceStore},
    ContainerId, EntityId, ReferenceId, Snippet, TextId,
};

mod canvas;
mod math;
mod snippet;

use math::MathRenderer;

const CANVAS_MARGIN: f32 = 0.0;
const CARD_WIDTH: f32 = 150.0;
const CARD_PADDING_H: f32 = 8.0;
const CARD_MARGIN_Y: f32 = 6.0;

pub(crate) struct HomePage {
    all_snippets: BTreeMap<EntityId, Snippet>,
    store: SnippetStore,
    workspace: Workspace,
    workspace_store: WorkspaceStore,
    root: ContainerCanvas,
    folder_views: BTreeMap<ContainerId, ContainerCanvas>,
    views: Vec<View>,
    next_view_id: u64,
    pending_delete: Option<PendingDelete>,
    rename_dialog: RenameDialogState,
    clipboard: Option<ClipboardEntry>,
    /// Shared local TeX-to-SVG renderer for previews and document embeds.
    math_renderer: MathRenderer,
    /// Reference drops consumed by `DropOnCanvas` commands this frame; used by
    /// [`HomePage::finalize_drops`] to decide whether a dangling drag should
    /// bounce back to its start position.
    consumed_drops: BTreeSet<(ContainerId, ReferenceId)>,
}

/// Display mode of a snippet viewport.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ViewMode {
    /// Raw markdown editing (single pane).
    Source,
    /// Rendered CommonMark preview (read-only).
    #[default]
    Preview,
}

impl ViewMode {
    /// Whether the viewport is currently showing the raw-markdown editor.
    fn is_editing(self) -> bool {
        self == ViewMode::Source
    }
}

/// State of the "Insert Link…" picker for a snippet viewport.
#[derive(Clone, Debug)]
struct LinkPicker {
    /// Char index in the note body where the link is inserted.
    cursor: usize,
    /// Substring filter over snippet titles.
    filter: String,
    /// Focus the filter field only once when the picker opens (IME-safe).
    focus_requested: bool,
    /// Insert an inline embed (`![title]({id}.md)`) instead of a plain link.
    embed: bool,
}

#[derive(Debug)]
struct View {
    id: u64,
    entity_id: EntityId,
    mode: ViewMode,
    /// Per-view CommonMark cache shared by every preview pane.
    markdown_cache: egui_commonmark::CommonMarkCache,
    /// Transient: request focus for the source editor next frame (set when
    /// entering `Source` from a preview action).
    focus_edit: bool,
    /// Transient: pointer position where the right-click view-mode menu should
    /// open (the preview pane, or the empty area below the editor), if any.
    mode_menu: Option<egui::Pos2>,
    /// Transient: open "Insert Link…" picker, if any.
    link_picker: Option<LinkPicker>,
    /// Transient: last broken-link click error (message + remaining frames),
    /// auto-dismissed after a short while.
    link_error: Option<(String, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
enum RenameTarget {
    Snippet {
        id: EntityId,
        origin: egui::ViewportId,
    },
    Folder {
        id: ContainerId,
        origin: egui::ViewportId,
    },
}

impl RenameTarget {
    fn origin(&self) -> egui::ViewportId {
        match self {
            RenameTarget::Snippet { origin, .. } | RenameTarget::Folder { origin, .. } => *origin,
        }
    }
}

/// State of the single in-flight rename dialog. The dialog is rendered inside
/// the viewport stored in [`RenameTarget::origin`] so that it appears in the
/// window that initiated the rename.
#[derive(Default)]
struct RenameDialogState {
    pending: Option<RenameTarget>,
    buffer: String,
    focus_requested: bool,
}

/// Semantics chosen when a card's reference is picked up via its context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClipboardSemantics {
    /// Create a new reference in the target container; the source stays.
    Link,
    /// Move the reference; it is removed from the source container.
    Move,
}

/// A reference picked from a card context menu (`Link` / `Move`), ready to be
/// pasted into any open canvas via its `Paste` context-menu entry.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClipboardEntry {
    source_container: ContainerId,
    reference_id: ReferenceId,
    target: ReferenceTarget,
    semantics: ClipboardSemantics,
    /// The viewport where the reference was picked up; the clipboard status
    /// indicator is shown in this window.
    origin: egui::ViewportId,
}

/// A card drag published to egui's drag-and-drop payload. Any canvas can be a
/// drop target: dropping on an empty canvas creates/moves a reference in that
/// container, dropping on a folder card targets that folder's container.
/// Must be `Send + Sync` for [`egui::DragAndDrop::set_payload`].
#[derive(Clone, Debug, PartialEq, Eq)]
enum DragPayload {
    Reference {
        source_container: ContainerId,
        reference_id: ReferenceId,
        target: ReferenceTarget,
    },
}

/// A pending "delete last reference" confirmation. Deleting the final link to
/// a snippet or folder permanently removes the underlying entity/container, so
/// a confirmation dialog is shown first.
#[derive(Clone, Debug, PartialEq)]
struct PendingDelete {
    owner: ContainerId,
    reference: ReferenceId,
    target: ReferenceTarget,
    /// The viewport that initiated the delete; the confirmation dialog is
    /// shown in this window.
    origin: egui::ViewportId,
}

#[derive(Clone, Debug, Default)]
enum ViewAction {
    #[default]
    None,
    Close,
    /// Open another snippet, requested by clicking a local markdown link in a
    /// preview pane.
    OpenSnippet(EntityId),
}

#[derive(Debug)]
struct CanvasItem {
    reference_id: ReferenceId,
    target: ReferenceTarget,
    position: [f32; 2],
    size: egui::Vec2,
}

#[derive(Clone, Debug)]
struct DragState {
    index: usize,
    start_position: [f32; 2],
    invalid: bool,
    /// Identity of the dragged reference; used to match the egui
    /// drag-and-drop payload and to resolve the card safely at frame end.
    reference_id: ReferenceId,
}

#[derive(Debug)]
struct ContainerCanvas {
    container_id: ContainerId,
    items: Vec<CanvasItem>,
    /// Canvas-local text annotations. Texts are not references: they cannot
    /// be linked, pasted, or moved across container boundaries.
    texts: Vec<CanvasText>,
    layout: ContainerLayout,
    dragging: Option<DragState>,
    /// Transient: index of the text being dragged, its start position, and
    /// whether the current position overlaps another element.
    dragging_text: Option<(usize, [f32; 2], bool)>,
    /// Transient: the text currently being edited in place, if any.
    editing_text: Option<TextId>,
    /// Transient: focus is requested only on the first frame of editing, so
    /// that IME input is not broken by repeated focus requests.
    edit_focus_requested: bool,
    /// Transient: canvas-local position of the right-click that opened the
    /// empty-canvas context menu; new items are created here. Stored so the
    /// position survives while the menu stays open across frames.
    menu_anchor: Option<[f32; 2]>,
}

#[derive(Debug)]
enum CanvasCommand {
    OpenSnippet(EntityId),
    OpenFolder(ContainerId),
    /// Remove a reference from `owner`. If it is the last link to its target,
    /// a confirmation dialog is shown before the entity/container is deleted.
    DeleteReference {
        owner: ContainerId,
        reference: ReferenceId,
        target: ReferenceTarget,
    },
    CloseFolder(ContainerId),
    RenameSnippet(EntityId),
    RenameFolder(ContainerId),
    /// Internal: apply the rename confirmed in a viewport's dialog.
    ApplyRename(RenameTarget),
    /// Internal: apply the delete confirmed in a viewport's dialog.
    ConfirmDelete(PendingDelete),
    /// Paste the clipboard reference into the given container.
    PasteClipboard {
        container: ContainerId,
        entry: ClipboardEntry,
    },
    /// Drop a dragged card onto this canvas: create (default) or move (Shift)
    /// the reference into `container`.
    DropOnCanvas {
        container: ContainerId,
        /// Drop position in the target canvas's local coordinates; `None`
        /// picks a free default slot (used when dropping onto a folder card).
        position: Option<[f32; 2]>,
        payload: DragPayload,
        move_semantics: bool,
    },
}

impl ContainerCanvas {
    fn save_layout(&mut self, store: &WorkspaceStore) {
        for item in &self.items {
            self.layout.items.insert(
                item.reference_id.clone(),
                CardLayout {
                    position: item.position,
                    color: None,
                },
            );
        }
        self.layout.texts = self.texts.clone();
        let _ = store.save_layout(&self.layout);
    }

    /// Repositions every card into an organized column-major grid: cards fill
    /// the first column top-to-bottom, then wrap to the next column. The number
    /// of rows per column is chosen so a column fits within `viewport_height`
    /// (the visible canvas height); when there are more cards than that, the
    /// layout overflows horizontally into the scroll area.
    fn organize(&mut self, store: &WorkspaceStore, viewport_height: f32) {
        const MARGIN: f32 = 24.0;
        const STEP_X: f32 = CARD_WIDTH + 24.0;
        const GAP_Y: f32 = 12.0;
        const MIN_CARD_HEIGHT: f32 = 25.0;

        let max_height = self
            .items
            .iter()
            .map(|item| item.size.y)
            .fold(MIN_CARD_HEIGHT, f32::max)
            .max(MIN_CARD_HEIGHT);
        let step_y = max_height + GAP_Y;
        let rows_per_column = if viewport_height <= MARGIN {
            1
        } else {
            (((viewport_height - MARGIN) / step_y).floor() as usize).max(1)
        };
        for (index, item) in self.items.iter_mut().enumerate() {
            let column = index / rows_per_column;
            let row = index % rows_per_column;
            item.position = [
                MARGIN + column as f32 * STEP_X,
                MARGIN + row as f32 * step_y,
            ];
        }
        self.save_layout(store);
    }

    fn remove_reference(&mut self, reference_id: &ReferenceId, store: &WorkspaceStore) {
        self.items.retain(|item| &item.reference_id != reference_id);
        self.layout.items.remove(reference_id);
        let _ = store.save_layout(&self.layout);
    }

    fn remove_entity(&mut self, entity_id: &EntityId, store: &WorkspaceStore) {
        let removed: Vec<_> = self
            .items
            .iter()
            .filter_map(|item| match &item.target {
                ReferenceTarget::Snippet(id) if id == entity_id => Some(item.reference_id.clone()),
                _ => None,
            })
            .collect();
        self.items.retain(
            |item| !matches!(&item.target, ReferenceTarget::Snippet(id) if id == entity_id),
        );
        for reference_id in removed {
            self.layout.items.remove(&reference_id);
        }
        let _ = store.save_layout(&self.layout);
    }
}

impl HomePage {
    pub(crate) fn new(workspace: impl Into<PathBuf>) -> Self {
        let workspace_path = workspace.into();
        let store = SnippetStore::open(&workspace_path).expect("failed to open snippet store");
        let mut snippets = store.load_all().unwrap_or_default();
        if snippets.is_empty() {
            for (title, content) in [
                ("hello", "hello, world!"),
                ("floatdea", "Welcome to floatdea!"),
                (
                    "help",
                    "Right-click for relevant operations.\n\nDouble-click on a card to open it. Drag cards to move them.",
                ),
            ] {
                let snippet = Snippet {
                    id: EntityId::new(),
                    title: title.to_owned(),
                    content: content.to_owned(),
                };
                let _ = store.save(&snippet);
                snippets.push(snippet);
            }
        }

        let workspace_store =
            WorkspaceStore::open(&workspace_path).expect("failed to open workspace metadata");
        let workspace = workspace_store
            .load_or_initialize(&snippets)
            .expect("failed to load workspace metadata");
        let all_snippets = snippets
            .into_iter()
            .map(|snippet| (snippet.id.clone(), snippet))
            .collect();
        let root = Self::load_container_canvas(
            &workspace,
            &workspace_store,
            &all_snippets,
            workspace.root.clone(),
        );

        Self {
            all_snippets,
            store,
            workspace,
            workspace_store,
            root,
            folder_views: BTreeMap::new(),
            views: Vec::new(),
            next_view_id: 0,
            pending_delete: None,
            rename_dialog: RenameDialogState::default(),
            clipboard: None,
            math_renderer: MathRenderer::default(),
            consumed_drops: BTreeSet::new(),
        }
    }

    fn load_container_canvas(
        workspace: &Workspace,
        store: &WorkspaceStore,
        snippets: &BTreeMap<EntityId, Snippet>,
        container_id: ContainerId,
    ) -> ContainerCanvas {
        let layout = store
            .load_layout(&container_id)
            .unwrap_or_else(|_| ContainerLayout::empty(container_id.clone()));
        let items = workspace
            .containers
            .get(&container_id)
            .into_iter()
            .flat_map(|container| &container.members)
            .filter(|reference| match &reference.target {
                ReferenceTarget::Snippet(id) => snippets.contains_key(id),
                ReferenceTarget::Container(id) => workspace.containers.contains_key(id),
            })
            .enumerate()
            .map(|(index, reference)| CanvasItem {
                reference_id: reference.id.clone(),
                target: reference.target.clone(),
                position: layout
                    .items
                    .get(&reference.id)
                    .map_or_else(|| default_card_position(index), |item| item.position),
                size: egui::vec2(CARD_WIDTH, 25.0),
            })
            .collect();

        ContainerCanvas {
            container_id,
            items,
            texts: layout.texts.clone(),
            layout,
            dragging: None,
            dragging_text: None,
            editing_text: None,
            edit_focus_requested: false,
            menu_anchor: None,
        }
    }

    fn open_view(&mut self, entity_id: EntityId) {
        // Opening a snippet is a navigation action; drop any pending clipboard
        // reference so a stale paste cannot leak into another window.
        self.clipboard = None;
        let id = self.next_view_id;
        self.next_view_id += 1;
        // Empty snippets open straight into the editor so they can be typed
        // into immediately; notes with content open as a rendered preview.
        let mode = if self
            .all_snippets
            .get(&entity_id)
            .is_some_and(|snippet| snippet.content.is_empty())
        {
            ViewMode::Source
        } else {
            ViewMode::Preview
        };
        self.views.push(View {
            id,
            entity_id,
            mode,
            markdown_cache: egui_commonmark::CommonMarkCache::default(),
            focus_edit: false,
            mode_menu: None,
            link_picker: None,
            link_error: None,
        });
    }

    fn process_canvas_commands(&mut self, commands: Vec<CanvasCommand>, origin: egui::ViewportId) {
        for command in commands {
            match command {
                CanvasCommand::OpenSnippet(id) => self.open_view(id),
                CanvasCommand::OpenFolder(id) => self.open_folder(&id),
                CanvasCommand::DeleteReference {
                    owner,
                    reference,
                    target,
                } => {
                    if reference_count(&self.workspace, &target) == 1 {
                        self.pending_delete = Some(PendingDelete {
                            owner,
                            reference,
                            target,
                            origin,
                        });
                    } else {
                        self.remove_reference_only(&owner, &reference);
                    }
                }
                CanvasCommand::CloseFolder(id) => {
                    let viewport =
                        egui::ViewportId::from_hash_of(("folder-view", id.as_str()));
                    self.clear_rename_for_viewport(viewport);
                    if self
                        .pending_delete
                        .as_ref()
                        .is_some_and(|pending| pending.origin == viewport)
                    {
                        self.pending_delete = None;
                    }
                    self.folder_views.remove(&id);
                }
                CanvasCommand::RenameSnippet(id) => {
                    if let Some(snippet) = self.all_snippets.get(&id) {
                        self.rename_dialog.buffer = snippet.title.clone();
                        self.rename_dialog.pending = Some(RenameTarget::Snippet { id, origin });
                        self.rename_dialog.focus_requested = false;
                    }
                }
                CanvasCommand::RenameFolder(id) => {
                    if let Some(container) = self.workspace.containers.get(&id) {
                        self.rename_dialog.buffer = container.title.clone();
                        self.rename_dialog.pending = Some(RenameTarget::Folder { id, origin });
                        self.rename_dialog.focus_requested = false;
                    }
                }
                CanvasCommand::ApplyRename(target) => {
                    if self.rename_dialog.pending.as_ref() != Some(&target) {
                        continue;
                    }
                    let new_title = self.rename_dialog.buffer.trim().to_owned();
                    let ok = match &target {
                        RenameTarget::Snippet { id, .. } => self.rename_snippet(id, new_title),
                        RenameTarget::Folder { id, .. } => self.rename_folder(id, new_title),
                    };
                    if ok {
                        self.rename_dialog.pending = None;
                    }
                }
                CanvasCommand::ConfirmDelete(pending) => {
                    if self.pending_delete.as_ref() == Some(&pending) {
                        self.confirm_delete(pending);
                    }
                }
                CanvasCommand::PasteClipboard { container, entry } => {
                    if self.paste_clipboard(&container, &entry) {
                        // The clipboard is single-use: clear it after a
                        // successful paste (Link or Move).
                        self.clipboard = None;
                    }
                }
                CanvasCommand::DropOnCanvas {
                    container,
                    position,
                    payload,
                    move_semantics,
                } => {
                    if self.apply_drop(&container, position, &payload, move_semantics)
                        && let DragPayload::Reference {
                            source_container,
                            reference_id,
                            ..
                        } = &payload
                    {
                        // Record the consumption so `finalize_drops` knows the
                        // drag ended in a successful drop (no bounce-back).
                        self.consumed_drops
                            .insert((source_container.clone(), reference_id.clone()));
                    }
                }
            }
        }
    }

    fn clear_rename_for_viewport(&mut self, viewport_id: egui::ViewportId) {
        if self
            .rename_dialog
            .pending
            .as_ref()
            .is_some_and(|target| target.origin() == viewport_id)
        {
            self.rename_dialog.pending = None;
        }
    }

    /// Removes a single reference from `owner` without touching the underlying
    /// entity/container (used when the deleted link is not the last one).
    fn remove_reference_only(&mut self, owner: &ContainerId, reference: &ReferenceId) {
        let _ = self.workspace.remove_reference(owner, reference);
        let _ = self.workspace_store.save(&self.workspace);
        if owner == &self.root.container_id {
            self.root.remove_reference(reference, &self.workspace_store);
        } else if let Some(canvas) = self.folder_views.get_mut(owner) {
            canvas.remove_reference(reference, &self.workspace_store);
        }
    }

    /// Confirms a "delete last reference" action: removes the link and deletes
    /// the underlying snippet file or folder container permanently.
    fn confirm_delete(&mut self, pending: PendingDelete) {
        match &pending.target {
            ReferenceTarget::Snippet(entity_id) => {
                let entity_id = entity_id.clone();
                if let Some(snippet) = self.all_snippets.get(&entity_id) {
                    let _ = self.store.remove(snippet);
                }
                self.workspace.remove_entity_references(&entity_id);
                let _ = self.workspace_store.save(&self.workspace);
                self.root.remove_entity(&entity_id, &self.workspace_store);
                for canvas in self.folder_views.values_mut() {
                    canvas.remove_entity(&entity_id, &self.workspace_store);
                }
                self.all_snippets.remove(&entity_id);
                self.views.retain(|view| view.entity_id != entity_id);
            }
            ReferenceTarget::Container(container_id) => {
                let container_id = container_id.clone();
                let _ = self
                    .workspace
                    .remove_reference(&pending.owner, &pending.reference);
                self.workspace.containers.remove(&container_id);
                let _ = self.workspace_store.save(&self.workspace);
                if pending.owner == self.root.container_id {
                    self.root
                        .remove_reference(&pending.reference, &self.workspace_store);
                } else if let Some(canvas) = self.folder_views.get_mut(&pending.owner) {
                    canvas.remove_reference(&pending.reference, &self.workspace_store);
                }
                self.clear_rename_for_viewport(egui::ViewportId::from_hash_of((
                    "folder-view",
                    container_id.as_str(),
                )));
                self.folder_views.remove(&container_id);
            }
        }
        self.pending_delete = None;
    }

    fn canvas_for(&self, container: &ContainerId) -> Option<&ContainerCanvas> {
        if container == &self.root.container_id {
            Some(&self.root)
        } else {
            self.folder_views.get(container)
        }
    }

    /// Applies a clipboard paste into `container`. Returns `false` when the
    /// operation is not allowed (self-reference, moving within the same
    /// container, or a stale target); the menu disables `Paste` in those cases.
    fn paste_clipboard(&mut self, container: &ContainerId, entry: &ClipboardEntry) -> bool {
        if !canvas::clipboard_valid_for(entry, container, &self.all_snippets, &self.workspace) {
            return false;
        }
        let new_reference_id = match &entry.target {
            ReferenceTarget::Snippet(entity_id) => {
                let Ok(id) = self
                    .workspace
                    .add_snippet_reference(container, entity_id.clone())
                else {
                    return false;
                };
                id
            }
            ReferenceTarget::Container(target_id) => {
                let Ok(id) = self
                    .workspace
                    .add_container_reference(container, target_id.clone())
                else {
                    return false;
                };
                id
            }
        };
        let position = self.canvas_for(container).map(|canvas| {
            canvas::default_position_for(
                &canvas.items,
                &self.all_snippets,
                &self.workspace,
                &canvas::approx_text_rects(&canvas.texts),
            )
        });
        let target_canvas = if container == &self.root.container_id {
            Some(&mut self.root)
        } else {
            self.folder_views.get_mut(container)
        };
        if let Some(canvas) = target_canvas {
            let position = position.unwrap_or_else(|| default_card_position(canvas.items.len()));
            canvas.items.push(CanvasItem {
                reference_id: new_reference_id.clone(),
                target: entry.target.clone(),
                position,
                size: egui::vec2(CARD_WIDTH, 25.0),
            });
            canvas.layout.items.insert(
                new_reference_id,
                CardLayout {
                    position,
                    color: None,
                },
            );
            canvas.save_layout(&self.workspace_store);
        }
        if matches!(entry.semantics, ClipboardSemantics::Move) {
            let _ = self
                .workspace
                .remove_reference(&entry.source_container, &entry.reference_id);
            let source_canvas = if entry.source_container == self.root.container_id {
                Some(&mut self.root)
            } else {
                self.folder_views.get_mut(&entry.source_container)
            };
            if let Some(canvas) = source_canvas {
                canvas
                    .items
                    .retain(|item| item.reference_id != entry.reference_id);
                canvas.layout.items.remove(&entry.reference_id);
                canvas.save_layout(&self.workspace_store);
            }
        }
        let _ = self.workspace_store.save(&self.workspace);
        true
    }

    /// Applies a drag-and-drop: adds a reference to `container` at `position`
    /// (or a free default slot when `None`), optionally removing the source
    /// reference (`move_semantics`). Returns `false` when the drop is not
    /// allowed (self-reference, move within the same container, stale target).
    fn apply_drop(
        &mut self,
        container: &ContainerId,
        position: Option<[f32; 2]>,
        payload: &DragPayload,
        move_semantics: bool,
    ) -> bool {
        let DragPayload::Reference {
            source_container,
            reference_id,
            target,
        } = payload;
        if !canvas::drop_valid_for(
            payload,
            container,
            &self.all_snippets,
            &self.workspace,
            move_semantics,
        ) {
            return false;
        }
        let new_reference_id = match target {
            ReferenceTarget::Snippet(entity_id) => {
                let Ok(id) = self
                    .workspace
                    .add_snippet_reference(container, entity_id.clone())
                else {
                    return false;
                };
                id
            }
            ReferenceTarget::Container(target_id) => {
                let Ok(id) = self
                    .workspace
                    .add_container_reference(container, target_id.clone())
                else {
                    return false;
                };
                id
            }
        };
        let position = match position {
            Some(position) => position,
            None => {
                let items: &[CanvasItem] = self
                    .canvas_for(container)
                    .map(|canvas| canvas.items.as_slice())
                    .unwrap_or(&[]);
                canvas::default_position_for(
                    items,
                    &self.all_snippets,
                    &self.workspace,
                    &[],
                )
            }
        };
        let target_canvas = if container == &self.root.container_id {
            Some(&mut self.root)
        } else {
            self.folder_views.get_mut(container)
        };
        if let Some(canvas) = target_canvas {
            canvas.items.push(CanvasItem {
                reference_id: new_reference_id.clone(),
                target: target.clone(),
                position,
                size: egui::vec2(CARD_WIDTH, 25.0),
            });
            canvas.layout.items.insert(
                new_reference_id.clone(),
                CardLayout {
                    position,
                    color: None,
                },
            );
            canvas.save_layout(&self.workspace_store);
        } else {
            // The target container is not open: persist just the layout entry.
            let mut layout = self
                .workspace_store
                .load_layout(container)
                .unwrap_or_else(|_| ContainerLayout::empty(container.clone()));
            layout.items.insert(
                new_reference_id.clone(),
                CardLayout {
                    position,
                    color: None,
                },
            );
            let _ = self.workspace_store.save_layout(&layout);
        }
        if move_semantics && source_container != container {
            let _ = self
                .workspace
                .remove_reference(source_container, reference_id);
            let source_canvas = if source_container == &self.root.container_id {
                Some(&mut self.root)
            } else {
                self.folder_views.get_mut(source_container)
            };
            if let Some(canvas) = source_canvas {
                canvas
                    .items
                    .retain(|item| &item.reference_id != reference_id);
                canvas.layout.items.remove(reference_id);
                canvas.save_layout(&self.workspace_store);
            }
        }
        let _ = self.workspace_store.save(&self.workspace);
        true
    }

    /// End-of-frame pass for cross-window drags. A drag that leaves its source
    /// canvas keeps the pointer "down" there (egui deliberately does not turn
    /// [`egui::Event::PointerGone`] into a release), so the source canvas never
    /// observes the release itself. The egui payload is the authoritative
    /// "still dragging" signal: once it is gone (consumed by a drop target or
    /// cleared by egui on release) the drag is finalized. Cards whose drop was
    /// not consumed bounce back to their start position.
    fn finalize_drops(&mut self, ctx: &egui::Context) {
        let payload = egui::DragAndDrop::payload::<DragPayload>(ctx)
            .map(|arc| (*arc).clone());
        let consumed = std::mem::take(&mut self.consumed_drops);
        let mut canvases: Vec<&mut ContainerCanvas> = std::iter::once(&mut self.root)
            .chain(self.folder_views.values_mut())
            .collect();
        for canvas in &mut canvases {
            let Some(drag) = canvas.dragging.clone() else {
                continue;
            };
            // While the payload still matches this drag, the user is still
            // dragging (the pointer may be over another window).
            let alive = payload.as_ref().is_some_and(|p| {
                matches!(p, DragPayload::Reference { reference_id, .. }
                    if reference_id == &drag.reference_id)
            });
            if alive {
                continue;
            }
            let card_present = drag.index < canvas.items.len()
                && canvas.items[drag.index].reference_id == drag.reference_id;
            let consumed_here = consumed.contains(&(
                canvas.container_id.clone(),
                drag.reference_id.clone(),
            ));
            if card_present && !consumed_here {
                // The drop was not consumed by any canvas: bounce back.
                canvas.items[drag.index].position = drag.start_position;
            }
            if card_present {
                canvas.save_layout(&self.workspace_store);
            }
            canvas.dragging = None;
        }
    }
}

/// Counts how many references across the whole workspace point at `target`.
/// A count of `1` means the link is the last one to that snippet/folder.
fn reference_count(workspace: &Workspace, target: &ReferenceTarget) -> usize {
    workspace
        .containers
        .values()
        .flat_map(|container| &container.members)
        .filter(|reference| &reference.target == target)
        .count()
}

fn default_card_position(index: usize) -> [f32; 2] {
    [
        24.0 + (index % 16) as f32 * 200.0,
        24.0 + (index / 16) as f32 * 130.0,
    ]
}

impl App for HomePage {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let root_viewport = ui.ctx().viewport_id();
        let root_commands = self.render_home_panel(ui);
        self.process_canvas_commands(root_commands, root_viewport);
        self.render_delete_dialog(ui);
        self.render_rename_dialog(ui);

        // Title index shared by the editor's "Insert Link…" picker, the paste
        // menu, and drag & drop into a note (looked up by id, so it stays
        // correct even while a snippet is mutably borrowed below).
        // (id, title, content) snapshot shared by link insertion and inline
        // embeds (id/content looked up by id, so it stays correct even while a
        // snippet is mutably borrowed below).
        let snippet_index: Vec<(EntityId, String, String)> = self
            .all_snippets
            .iter()
            .map(|(id, snippet)| (id.clone(), snippet.title.clone(), snippet.content.clone()))
            .collect();
        let mut closed_views = Vec::new();
        let mut open_views = Vec::new();
        for view in &mut self.views {
            let Some(item) = self.all_snippets.get_mut(&view.entity_id) else {
                continue;
            };
            match Self::render_snippet_viewport(
                ui,
                view,
                item,
                &self.store,
                &snippet_index,
                &self.clipboard,
                &self.math_renderer,
            ) {
                ViewAction::Close => closed_views.push(view.id),
                ViewAction::OpenSnippet(id) => open_views.push(id),
                ViewAction::None => {}
            }
        }
        self.views.retain(|view| !closed_views.contains(&view.id));
        for id in open_views {
            if self.all_snippets.contains_key(&id) {
                self.open_view(id);
            }
        }

        let mut commands_by_viewport: Vec<(egui::ViewportId, Vec<CanvasCommand>)> = Vec::new();
        for (container_id, canvas) in &mut self.folder_views {
            let title = self
                .workspace
                .containers
                .get(container_id)
                .map(|container| container.title.clone())
                .unwrap_or_default();
            let viewport_id =
                egui::ViewportId::from_hash_of(("folder-view", container_id.as_str()));
            let commands = Self::render_folder_viewport(
                ui,
                canvas,
                &title,
                &mut self.workspace,
                &self.workspace_store,
                &self.store,
                &mut self.all_snippets,
                &mut self.rename_dialog,
                &mut self.pending_delete,
                &mut self.clipboard,
            );
            commands_by_viewport.push((viewport_id, commands));
        }
        for (viewport_id, commands) in commands_by_viewport {
            self.process_canvas_commands(commands, viewport_id);
        }
        // Finalize any cross-window drag whose payload was consumed or cleared
        // during this frame (bouncing back un-consumed drops).
        self.finalize_drops(ui.ctx());
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    struct TestFolder(PathBuf);

    impl TestFolder {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(
                std::env::temp_dir()
                    .join(format!("floatdea-home-page-{}-{nonce}", std::process::id())),
            )
        }
    }

    impl Drop for TestFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn restores_root_card_positions() {
        let folder = TestFolder::new();
        let mut first_session = HomePage::new(&folder.0);
        first_session.root.items[0].position = [137.0, 281.0];
        first_session
            .root
            .save_layout(&first_session.workspace_store);

        let second_session = HomePage::new(&folder.0);

        assert_eq!(second_session.root.items[0].position, [137.0, 281.0]);
    }

    #[test]
    fn restores_snippet_and_folder_positions_from_one_layout() {
        let folder = TestFolder::new();
        let mut first_session = HomePage::new(&folder.0);
        let folder_id = first_session.workspace.create_container("Folder");
        let reference_id = first_session
            .workspace
            .add_container_to_root(folder_id.clone());
        first_session
            .workspace_store
            .save(&first_session.workspace)
            .unwrap();
        first_session.root.items.push(CanvasItem {
            reference_id,
            target: ReferenceTarget::Container(folder_id),
            position: [320.0, 144.0],
            size: egui::vec2(CARD_WIDTH, 25.0),
        });
        first_session
            .root
            .save_layout(&first_session.workspace_store);

        let second_session = HomePage::new(&folder.0);

        assert!(second_session.root.items.iter().any(|item| {
            matches!(item.target, ReferenceTarget::Container(_)) && item.position == [320.0, 144.0]
        }));
    }

    fn clip_root_first_entry(page: &HomePage, semantics: ClipboardSemantics) -> ClipboardEntry {
        ClipboardEntry {
            source_container: page.root.container_id.clone(),
            reference_id: page.root.items[0].reference_id.clone(),
            target: page.root.items[0].target.clone(),
            semantics,
            origin: egui::ViewportId::ROOT,
        }
    }

    #[test]
    fn pastes_link_reference_into_folder() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);

        let entry = clip_root_first_entry(&page, ClipboardSemantics::Link);
        let root_count = page.root.items.len();
        assert!(page.paste_clipboard(&folder_id, &entry));
        assert_eq!(
            page.root.items.len(),
            root_count,
            "link keeps the source card"
        );
        let folder_canvas = page.folder_views.get(&folder_id).expect("folder view open");
        assert_eq!(folder_canvas.items.len(), 1);
        assert_eq!(folder_canvas.items[0].target, entry.target);
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
    }

    #[test]
    fn pastes_move_reference_into_folder() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);

        let entry = clip_root_first_entry(&page, ClipboardSemantics::Move);
        let root_count = page.root.items.len();
        assert!(page.paste_clipboard(&folder_id, &entry));
        assert_eq!(
            page.root.items.len(),
            root_count - 1,
            "move removes the source card"
        );
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
        // The entity itself is untouched; only the reference moved.
        assert!(page.all_snippets.contains_key(match &entry.target {
            ReferenceTarget::Snippet(id) => id,
            ReferenceTarget::Container(_) => panic!("root cards are snippets"),
        }));
    }

    #[test]
    fn pasted_reference_survives_restart() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        let entry = clip_root_first_entry(&page, ClipboardSemantics::Link);
        assert!(page.paste_clipboard(&folder_id, &entry));
        drop(page);

        let reloaded = HomePage::new(&folder.0);
        assert_eq!(reloaded.workspace.containers[&folder_id].members.len(), 1);
    }

    #[test]
    fn rejects_self_reference_paste() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let reference_id = page.workspace.add_container_to_root(folder_id.clone());
        let entry = ClipboardEntry {
            source_container: page.root.container_id.clone(),
            reference_id,
            target: ReferenceTarget::Container(folder_id.clone()),
            semantics: ClipboardSemantics::Link,
            origin: egui::ViewportId::ROOT,
        };
        assert!(!page.paste_clipboard(&folder_id, &entry));
    }

    #[test]
    fn drop_creates_link_reference_in_folder() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);

        let (entity_id, reference_id) = root_first_snippet(&page);
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id,
            target: ReferenceTarget::Snippet(entity_id),
        };
        let root_count = page.root.items.len();
        assert!(page.apply_drop(&folder_id, Some([88.0, 44.0]), &payload, false));
        // Link keeps the source card.
        assert_eq!(page.root.items.len(), root_count);
        let folder_canvas = page.folder_views.get(&folder_id).expect("folder view open");
        assert_eq!(folder_canvas.items.len(), 1);
        assert_eq!(folder_canvas.items[0].position, [88.0, 44.0]);
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
    }

    #[test]
    fn drop_moves_reference_into_folder() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);

        let (entity_id, reference_id) = root_first_snippet(&page);
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id,
            target: ReferenceTarget::Snippet(entity_id.clone()),
        };
        let root_count = page.root.items.len();
        assert!(page.apply_drop(&folder_id, None, &payload, true));
        assert_eq!(
            page.root.items.len(),
            root_count - 1,
            "move removes the source card"
        );
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
        // The entity itself is untouched; only the reference moved.
        assert!(page.all_snippets.contains_key(&entity_id));
    }

    #[test]
    fn drop_rejects_self_reference() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let reference_id = page.workspace.add_container_to_root(folder_id.clone());
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id,
            target: ReferenceTarget::Container(folder_id.clone()),
        };
        assert!(!page.apply_drop(&folder_id, None, &payload, false));
    }

    #[test]
    fn drop_rejects_move_into_same_container() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (entity_id, reference_id) = root_first_snippet(&page);
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id,
            target: ReferenceTarget::Snippet(entity_id),
        };
        let root_id = page.root.container_id.clone();
        let root_count = page.root.items.len();
        assert!(!page.apply_drop(&root_id, Some([24.0, 24.0]), &payload, true));
        assert_eq!(page.root.items.len(), root_count);
    }

    #[test]
    fn drop_command_records_consumption() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        let (entity_id, reference_id) = root_first_snippet(&page);
        let payload = DragPayload::Reference {
            source_container: page.root.container_id.clone(),
            reference_id: reference_id.clone(),
            target: ReferenceTarget::Snippet(entity_id),
        };
        page.process_canvas_commands(
            vec![CanvasCommand::DropOnCanvas {
                container: folder_id.clone(),
                position: None,
                payload,
                move_semantics: false,
            }],
            egui::ViewportId::ROOT,
        );
        assert!(page
            .consumed_drops
            .contains(&(page.root.container_id.clone(), reference_id)));
        assert_eq!(page.workspace.containers[&folder_id].members.len(), 1);
    }

    #[test]
    fn rejects_move_into_same_container() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let entry = clip_root_first_entry(&page, ClipboardSemantics::Move);
        let root_count = page.root.items.len();
        let root_id = page.root.container_id.clone();
        assert!(!page.paste_clipboard(&root_id, &entry));
        assert_eq!(page.root.items.len(), root_count);
    }

    #[test]
    fn link_paste_command_clears_clipboard() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);
        let entry = clip_root_first_entry(&page, ClipboardSemantics::Link);
        page.clipboard = Some(entry.clone());
        page.process_canvas_commands(
            vec![CanvasCommand::PasteClipboard {
                container: folder_id,
                entry,
            }],
            egui::ViewportId::ROOT,
        );
        assert!(page.clipboard.is_none());
    }

    #[test]
    fn opening_snippet_clears_clipboard() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let entry = clip_root_first_entry(&page, ClipboardSemantics::Link);
        page.clipboard = Some(entry);
        let snippet_id = match &page.root.items[0].target {
            ReferenceTarget::Snippet(id) => id.clone(),
            ReferenceTarget::Container(_) => panic!("root cards are snippets"),
        };
        page.open_view(snippet_id);
        assert!(page.clipboard.is_none());
    }

    #[test]
    fn organize_arranges_cards_column_major_within_viewport() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        // Scatter the cards off-grid.
        for (index, item) in page.root.items.iter_mut().enumerate() {
            item.position = [1000.0 + index as f32, 1000.0];
        }
        let viewport_height = 480.0;
        page.root.organize(&page.workspace_store, viewport_height);

        // Mirror the layout math used by `organize` to derive the expected grid.
        const MARGIN: f32 = 24.0;
        const STEP_X: f32 = CARD_WIDTH + 24.0;
        const GAP_Y: f32 = 12.0;
        const MIN_CARD_HEIGHT: f32 = 25.0;
        let max_height = page
            .root
            .items
            .iter()
            .map(|item| item.size.y)
            .fold(MIN_CARD_HEIGHT, f32::max)
            .max(MIN_CARD_HEIGHT);
        let step_y = max_height + GAP_Y;
        let rows_per_column = (((viewport_height - MARGIN) / step_y).floor() as usize).max(1);

        for (index, item) in page.root.items.iter().enumerate() {
            let column = index / rows_per_column;
            let row = index % rows_per_column;
            assert_eq!(
                item.position,
                [
                    MARGIN + column as f32 * STEP_X,
                    MARGIN + row as f32 * step_y
                ]
            );
        }
        // The first column never exceeds the viewport height.
        let first_column_bottom = MARGIN
            + (page.root.items.len().min(rows_per_column).saturating_sub(1)) as f32 * step_y
            + max_height;
        assert!(first_column_bottom <= viewport_height);

        // The grid layout is persisted across sessions.
        let reloaded = HomePage::new(&folder.0);
        for (index, item) in reloaded.root.items.iter().enumerate() {
            let column = index / rows_per_column;
            let row = index % rows_per_column;
            assert_eq!(
                item.position,
                [
                    MARGIN + column as f32 * STEP_X,
                    MARGIN + row as f32 * step_y
                ]
            );
        }
    }

    fn root_first_snippet(page: &HomePage) -> (EntityId, ReferenceId) {
        let entity_id = match &page.root.items[0].target {
            ReferenceTarget::Snippet(id) => id.clone(),
            ReferenceTarget::Container(_) => panic!("root cards are snippets"),
        };
        (entity_id, page.root.items[0].reference_id.clone())
    }

    #[test]
    fn delete_reference_when_not_last_link_removes_only_the_link() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let _ = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);

        // Give the folder a second link to the same snippet so it is not the last one.
        let (entity_id, root_ref) = root_first_snippet(&page);
        let folder_ref = page
            .workspace
            .add_snippet_reference(&folder_id, entity_id.clone())
            .unwrap();
        page.workspace_store.save(&page.workspace).unwrap();
        page.folder_views
            .get_mut(&folder_id)
            .unwrap()
            .items
            .push(CanvasItem {
                reference_id: folder_ref,
                target: ReferenceTarget::Snippet(entity_id.clone()),
                position: [24.0, 24.0],
                size: egui::vec2(CARD_WIDTH, 25.0),
            });

        let root_count = page.root.items.len();
        page.process_canvas_commands(
            vec![CanvasCommand::DeleteReference {
                owner: page.root.container_id.clone(),
                reference: root_ref,
                target: ReferenceTarget::Snippet(entity_id.clone()),
            }],
            egui::ViewportId::ROOT,
        );

        // Not the last link → no confirmation dialog, entity untouched.
        assert!(page.pending_delete.is_none());
        assert_eq!(page.root.items.len(), root_count - 1);
        assert!(page.folder_views[&folder_id]
            .items
            .iter()
            .any(|item| item.target == ReferenceTarget::Snippet(entity_id.clone())));
        assert!(page.all_snippets.contains_key(&entity_id));
    }

    #[test]
    fn delete_reference_when_last_link_asks_then_confirm_removes_entity() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let (entity_id, root_ref) = root_first_snippet(&page);
        let root_count = page.root.items.len();

        page.process_canvas_commands(
            vec![CanvasCommand::DeleteReference {
                owner: page.root.container_id.clone(),
                reference: root_ref,
                target: ReferenceTarget::Snippet(entity_id.clone()),
            }],
            egui::ViewportId::ROOT,
        );

        // Last link → confirmation dialog pending; nothing removed yet.
        assert!(page.pending_delete.is_some());
        assert_eq!(page.root.items.len(), root_count);
        assert!(page.all_snippets.contains_key(&entity_id));

        page.confirm_delete(page.pending_delete.clone().unwrap());
        assert!(page.pending_delete.is_none());
        assert!(!page.all_snippets.contains_key(&entity_id));
        assert_eq!(page.root.items.len(), root_count - 1);
    }

    #[test]
    fn confirm_delete_last_folder_link_removes_container_and_window() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        let folder_id = page.workspace.create_container("Folder");
        let reference_id = page.workspace.add_container_to_root(folder_id.clone());
        page.open_folder(&folder_id);
        assert!(page.folder_views.contains_key(&folder_id));

        page.confirm_delete(PendingDelete {
            owner: page.root.container_id.clone(),
            reference: reference_id,
            target: ReferenceTarget::Container(folder_id.clone()),
            origin: egui::ViewportId::ROOT,
        });

        assert!(!page.workspace.containers.contains_key(&folder_id));
        assert!(!page.folder_views.contains_key(&folder_id));
    }

    #[test]
    fn create_text_enters_edit_and_persists_across_sessions() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        {
            let mut data = canvas::CanvasData::new(
                &mut page.all_snippets,
                &mut page.workspace,
                &page.workspace_store,
                &page.store,
                &mut page.clipboard,
            );
            canvas::create_text(&mut page.root, &mut data, [24.0, 36.0]);

            assert_eq!(page.root.texts.len(), 1);
            let text_id = page.root.texts[0].id.clone();
            assert_eq!(page.root.editing_text, Some(text_id));
        }
        drop(page);

        let reloaded = HomePage::new(&folder.0);
        assert_eq!(reloaded.root.texts.len(), 1);
        assert_eq!(reloaded.root.texts[0].position, [24.0, 36.0]);
        assert!(reloaded.root.texts[0].text.is_empty());
    }

    #[test]
    fn delete_text_removes_it_and_clears_edit_state() {
        let folder = TestFolder::new();
        let mut page = HomePage::new(&folder.0);
        {
            let mut data = canvas::CanvasData::new(
                &mut page.all_snippets,
                &mut page.workspace,
                &page.workspace_store,
                &page.store,
                &mut page.clipboard,
            );
            canvas::create_text(&mut page.root, &mut data, [24.0, 24.0]);
            let text_id = page.root.texts[0].id.clone();

            canvas::delete_text(&mut page.root, &mut data, &text_id);

            assert!(page.root.texts.is_empty());
            assert!(page.root.editing_text.is_none());
        }
        drop(page);

        let reloaded = HomePage::new(&folder.0);
        assert!(reloaded.root.texts.is_empty());
    }
}
