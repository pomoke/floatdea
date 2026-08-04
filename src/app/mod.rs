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
}
