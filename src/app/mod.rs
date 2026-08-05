use std::{collections::BTreeMap, path::PathBuf};

use eframe::{App, egui};

use floatdea::data::{
    ContainerId, EntityId, ReferenceId, Snippet,
    storage::SnippetStore,
    workspace::{CardLayout, ContainerLayout, ReferenceTarget, Workspace, WorkspaceStore},
};

mod canvas;
mod snippet;

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
    pending_delete: Option<EntityId>,
    rename_dialog: RenameDialogState,
    clipboard: Option<ClipboardEntry>,
    paste_feedback: Option<String>,
}

#[derive(Debug)]
struct View {
    id: u64,
    entity_id: EntityId,
    editable: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum RenameTarget {
    Snippet { id: EntityId, origin: egui::ViewportId },
    Folder { id: ContainerId, origin: egui::ViewportId },
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
}

#[derive(Clone, Copy, Debug, Default)]
enum ViewAction {
    #[default]
    None,
    Close,
}

#[derive(Debug)]
struct CanvasItem {
    reference_id: ReferenceId,
    target: ReferenceTarget,
    position: [f32; 2],
    size: egui::Vec2,
}

#[derive(Clone, Copy, Debug)]
struct DragState {
    index: usize,
    start_position: [f32; 2],
    invalid: bool,
}

#[derive(Debug)]
struct ContainerCanvas {
    container_id: ContainerId,
    items: Vec<CanvasItem>,
    layout: ContainerLayout,
    dragging: Option<DragState>,
}

#[derive(Debug)]
enum CanvasCommand {
    OpenSnippet(EntityId),
    DeleteSnippet(EntityId),
    OpenFolder(ContainerId),
    RemoveFolder {
        owner: ContainerId,
        reference: ReferenceId,
        target: ContainerId,
    },
    CloseFolder(ContainerId),
    RenameSnippet(EntityId),
    RenameFolder(ContainerId),
    /// Internal: apply the rename confirmed in a viewport's dialog.
    ApplyRename(RenameTarget),
    /// Paste the clipboard reference into the given container.
    PasteClipboard {
        container: ContainerId,
        entry: ClipboardEntry,
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
            paste_feedback: None,
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
            layout,
            dragging: None,
        }
    }

    fn open_view(&mut self, entity_id: EntityId) {
        // Opening a snippet is a navigation action; drop any pending clipboard
        // reference so a stale paste cannot leak into another window.
        self.clipboard = None;
        let id = self.next_view_id;
        self.next_view_id += 1;
        let editable = self
            .all_snippets
            .get(&entity_id)
            .is_some_and(|snippet| snippet.content.is_empty());
        self.views.push(View {
            id,
            entity_id,
            editable,
        });
    }

    fn process_canvas_commands(&mut self, commands: Vec<CanvasCommand>, origin: egui::ViewportId) {
        for command in commands {
            match command {
                CanvasCommand::OpenSnippet(id) => self.open_view(id),
                CanvasCommand::DeleteSnippet(id) => self.pending_delete = Some(id),
                CanvasCommand::OpenFolder(id) => self.open_folder(&id),
                CanvasCommand::CloseFolder(id) => {
                    self.clear_rename_for_viewport(egui::ViewportId::from_hash_of((
                        "folder-view",
                        id.as_str(),
                    )));
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
                CanvasCommand::RemoveFolder {
                    owner,
                    reference,
                    target,
                } => {
                    let _ = self.workspace.remove_reference(&owner, &reference);
                    let _ = self.workspace_store.save(&self.workspace);
                    if owner == self.root.container_id {
                        self.root
                            .remove_reference(&reference, &self.workspace_store);
                    } else if let Some(canvas) = self.folder_views.get_mut(&owner) {
                        canvas.remove_reference(&reference, &self.workspace_store);
                    }
                    self.clear_rename_for_viewport(egui::ViewportId::from_hash_of((
                        "folder-view",
                        target.as_str(),
                    )));
                    self.folder_views.remove(&target);
                }
                CanvasCommand::PasteClipboard { container, entry } => {
                    if self.paste_clipboard(&container, &entry) {
                        // The clipboard is single-use: clear it after a
                        // successful paste (Link or Move).
                        self.clipboard = None;
                    } else {
                        self.paste_feedback = Some("Paste rejected".to_owned());
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
                let Ok(id) = self.workspace.add_snippet_reference(container, entity_id.clone())
                else {
                    return false;
                };
                id
            }
            ReferenceTarget::Container(target_id) => {
                let Ok(id) = self.workspace.add_container_reference(container, target_id.clone())
                else {
                    return false;
                };
                id
            }
        };
        let position = self
            .canvas_for(container)
            .map(|canvas| {
                canvas::default_position_for(&canvas.items, &self.all_snippets, &self.workspace)
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
                canvas.items.retain(|item| item.reference_id != entry.reference_id);
                canvas.layout.items.remove(&entry.reference_id);
                canvas.save_layout(&self.workspace_store);
            }
        }
        let _ = self.workspace_store.save(&self.workspace);
        true
    }

    /// Renders the cross-window clipboard status in the root viewport.
    fn render_clipboard_status(&mut self, ui: &mut egui::Ui) {
        let Some(entry) = self.clipboard.clone() else {
            self.paste_feedback = None;
            return;
        };
        let title = match &entry.target {
            ReferenceTarget::Snippet(id) => self
                .all_snippets
                .get(id)
                .map(|snippet| snippet.title.clone())
                .unwrap_or_else(|| "?".to_owned()),
            ReferenceTarget::Container(id) => self
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
        let feedback = self.paste_feedback.take();
        let mut text = format!("Clipboard: {title} ({verb})");
        if let Some(feedback) = &feedback {
            text.push_str(&format!("  |  {feedback}"));
        }
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
            self.clipboard = None;
        }
    }
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
        self.render_clipboard_status(ui);

        let mut closed_views = Vec::new();
        for view in &mut self.views {
            let Some(item) = self.all_snippets.get_mut(&view.entity_id) else {
                continue;
            };
            if matches!(
                Self::render_snippet_viewport(ui, view, item, &self.store),
                ViewAction::Close
            ) {
                closed_views.push(view.id);
            }
        }
        self.views.retain(|view| !closed_views.contains(&view.id));

        let mut commands_by_viewport: Vec<(egui::ViewportId, Vec<CanvasCommand>)> = Vec::new();
        for (container_id, canvas) in &mut self.folder_views {
            let title = self
                .workspace
                .containers
                .get(container_id)
                .map(|container| container.title.clone())
                .unwrap_or_default();
            let viewport_id = egui::ViewportId::from_hash_of(("folder-view", container_id.as_str()));
            let commands = Self::render_folder_viewport(
                ui,
                canvas,
                &title,
                &mut self.workspace,
                &self.workspace_store,
                &self.store,
                &mut self.all_snippets,
                &mut self.rename_dialog,
                &mut self.clipboard,
            );
            commands_by_viewport.push((viewport_id, commands));
        }
        for (viewport_id, commands) in commands_by_viewport {
            self.process_canvas_commands(commands, viewport_id);
        }
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
        assert_eq!(page.root.items.len(), root_count, "link keeps the source card");
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
        assert_eq!(page.root.items.len(), root_count - 1, "move removes the source card");
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
        };
        assert!(!page.paste_clipboard(&folder_id, &entry));
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
                [MARGIN + column as f32 * STEP_X, MARGIN + row as f32 * step_y]
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
                [MARGIN + column as f32 * STEP_X, MARGIN + row as f32 * step_y]
            );
        }
    }
}
