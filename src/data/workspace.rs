use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{
    AttachmentId, ContainerId, ConversationId, EntityId, ExternalFileId, ReferenceId, Snippet,
    TextId,
};

const WORKSPACE_VERSION: u32 = 1;
const LAYOUT_VERSION: u32 = 1;

/// The kind of a container. Ordinary containers and AI workspaces share the
/// same canvas/layout/reference machinery; the kind only selects AI-specific
/// member roles and the per-box AI sidecar store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerKind {
    /// A regular knowledge container.
    #[default]
    Normal,
    /// An AI workspace: a bounded workbench whose linked members are read-only
    /// sources, conversation cards, and user-confirmed outputs.
    AiWorkspace,
}

/// The role a member reference plays inside an AI workspace. Roles are
/// meaningful only inside AI boxes; ordinary containers always use `Normal`.
/// Roles are stored explicitly so card visuals, context menus and permissions
/// never depend on guessing from titles, colors or filenames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberRole {
    #[default]
    Normal,
    /// A linked, read-only context source inside an AI box. The AI box never
    /// holds the source entity's lifecycle: removal is always an unlink.
    Source,
    /// A user-confirmed AI answer saved as a regular snippet.
    Output,
    /// A conversation card inside an AI box (sidecar state, not Markdown).
    Conversation,
    /// A transient, unsaved AI result (reserved; draft cards are not yet
    /// rendered on the canvas).
    Draft,
}

/// A built-in, system-owned special item. Instances are permanent in their
/// container: they can be dragged around but never deleted or linked.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecialKind {
    /// Opens the system settings page.
    Settings,
}

impl SpecialKind {
    /// Human-readable label shown on the card.
    pub fn label(&self) -> &'static str {
        match self {
            SpecialKind::Settings => "Settings",
        }
    }
}

/// A reference to a file outside the workspace (PDF, Markdown, …). FloatDea
/// records only the absolute path and a display title; the file itself is
/// never imported or copied. Clicking the card opens it with the operating
/// system's default application, and removing the card never deletes the file.
///
/// When `media_type` is set to an image type (e.g. `"image/jpeg"`) the canvas
/// renders the file content directly as an image instead of showing a card.
/// This is used for source files that exceed the managed inline size limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalFileRef {
    /// Stable identity of the file link. Cards that link the same file share
    /// one id so renaming the display title updates every card at once.
    pub id: ExternalFileId,
    /// Absolute path of the file on disk.
    pub path: String,
    /// Display title shown on the canvas card (defaults to the file stem).
    pub title: String,
    /// MIME type hint for the file content. When `Some("image/…")` the canvas
    /// renders the file as an image rather than as a card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

/// How an image is fitted inside its canvas display rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFit {
    /// The entire image is visible, letter-boxed if the aspect ratio differs.
    #[default]
    Contain,
    /// The image fills the rectangle, cropping the longer dimension.
    Cover,
}

/// A managed image attachment stored in the workspace's `attachments/`
/// directory. The image is copied from its source, verified, and assigned a
/// stable identity. Multiple canvas references can point to the same
/// `ImageAttachment` without duplicating the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageAttachment {
    pub id: AttachmentId,
    /// Workspace-relative path, always `attachments/{id}.{ext}`.
    pub relative_path: String,
    /// Display title (defaults to the source file stem). Renaming the title
    /// does not change the file name on disk.
    pub title: String,
    /// MIME type, e.g. `"image/jpeg"` or `"image/png"`.
    pub media_type: String,
    /// File size in bytes.
    pub byte_len: u64,
    /// SHA-256 hex digest of the file content.
    pub content_hash: String,
    /// Pixel dimensions `[width, height]` after applying EXIF orientation.
    pub pixel_size: [u32; 2],
    /// Original source file stem before any sanitisation, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ReferenceTarget {
    Snippet(EntityId),
    Container(ContainerId),
    Special(SpecialKind),
    /// A conversation card inside an AI workspace. The conversation's actual
    /// state (title, messages, source bindings) lives in the AI sidecar store
    /// keyed by `ConversationId`; the card itself is a plain member reference
    /// so it reuses the existing canvas/layout persistence.
    Conversation(ConversationId),
    /// A card that opens an external file with the system's default app.
    ExternalFile(ExternalFileRef),
    /// A managed image attachment displayed directly on the canvas.
    Image(AttachmentId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    pub id: ReferenceId,
    pub target: ReferenceTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default)]
    pub presentation: ReferencePresentation,
    /// The role this reference plays inside an AI workspace. Defaults to
    /// `Normal` so older workspace files keep loading unchanged.
    #[serde(default)]
    pub role: MemberRole,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferencePresentation {
    #[default]
    Card,
    Title,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Container {
    pub id: ContainerId,
    pub title: String,
    #[serde(default)]
    pub kind: ContainerKind,
    #[serde(default)]
    pub members: Vec<Reference>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub version: u32,
    pub root: ContainerId,
    pub containers: BTreeMap<ContainerId, Container>,
    #[serde(default)]
    pub images: BTreeMap<AttachmentId, ImageAttachment>,
}

impl Workspace {
    pub fn empty() -> Self {
        let root = ContainerId::new();
        let container = Container {
            id: root.clone(),
            title: "Home".to_owned(),
            kind: ContainerKind::Normal,
            members: Vec::new(),
        };
        Self {
            version: WORKSPACE_VERSION,
            root: root.clone(),
            containers: BTreeMap::from([(root, container)]),
            images: BTreeMap::new(),
        }
    }

    pub fn root(&self) -> &Container {
        self.containers
            .get(&self.root)
            .expect("workspace root container must exist")
    }

    pub fn create_container(&mut self, title: impl Into<String>) -> ContainerId {
        let id = ContainerId::new();
        self.containers.insert(
            id.clone(),
            Container {
                id: id.clone(),
                title: title.into(),
                kind: ContainerKind::Normal,
                members: Vec::new(),
            },
        );
        id
    }

    /// Creates a new AI workspace container. AI boxes are ordinary containers
    /// with `ContainerKind::AiWorkspace`; the workspace may hold zero or more.
    pub fn create_ai_box(&mut self, title: impl Into<String>) -> ContainerId {
        let id = ContainerId::new();
        self.containers.insert(
            id.clone(),
            Container {
                id: id.clone(),
                title: title.into(),
                kind: ContainerKind::AiWorkspace,
                members: Vec::new(),
            },
        );
        id
    }

    /// Whether `container` exists and is an AI workspace.
    pub fn is_ai_box(&self, container: &ContainerId) -> bool {
        self.containers
            .get(container)
            .is_some_and(|container| container.kind == ContainerKind::AiWorkspace)
    }

    /// Adds a member marked as a read-only `Source` role (AI box context).
    pub fn add_source_reference(
        &mut self,
        container: &ContainerId,
        target: ReferenceTarget,
    ) -> io::Result<ReferenceId> {
        self.add_reference_with_role(container, target, MemberRole::Source)
    }

    /// Adds a `Conversation` card to an AI box. The conversation's sidecar data
    /// must be created by the AI store before the card is visible.
    pub fn add_conversation_card(
        &mut self,
        container: &ContainerId,
        conversation: ConversationId,
    ) -> io::Result<ReferenceId> {
        self.add_reference_with_role(
            container,
            ReferenceTarget::Conversation(conversation),
            MemberRole::Conversation,
        )
    }

    /// Adds an `Output` reference to an AI box, pointing at a regular snippet
    /// created by "Save as Snippet".
    pub fn add_output_reference(
        &mut self,
        container: &ContainerId,
        entity: EntityId,
    ) -> io::Result<ReferenceId> {
        self.add_reference_with_role(
            container,
            ReferenceTarget::Snippet(entity),
            MemberRole::Output,
        )
    }

    pub fn add_snippet_reference(
        &mut self,
        container: &ContainerId,
        entity: EntityId,
    ) -> io::Result<ReferenceId> {
        self.add_reference(container, ReferenceTarget::Snippet(entity))
    }

    pub fn add_container_reference(
        &mut self,
        container: &ContainerId,
        target: ContainerId,
    ) -> io::Result<ReferenceId> {
        if !self.containers.contains_key(&target) {
            return Err(invalid_data("reference targets a missing container"));
        }
        self.add_reference(container, ReferenceTarget::Container(target))
    }

    pub fn add_external_file_reference(
        &mut self,
        container: &ContainerId,
        file: ExternalFileRef,
    ) -> io::Result<ReferenceId> {
        self.add_reference(container, ReferenceTarget::ExternalFile(file))
    }

    /// Registers a managed image attachment in the workspace. The image file
    /// must already be present at `attachments/{id}.{ext}`.
    pub fn add_image(&mut self, image: ImageAttachment) -> io::Result<AttachmentId> {
        let id = image.id.clone();
        if self.images.contains_key(&id) {
            return Err(invalid_data("image attachment id already exists"));
        }
        self.images.insert(id.clone(), image);
        Ok(id)
    }

    /// Creates a new reference pointing to an existing image attachment.
    pub fn add_image_reference(
        &mut self,
        container: &ContainerId,
        image: AttachmentId,
    ) -> io::Result<ReferenceId> {
        if !self.images.contains_key(&image) {
            return Err(invalid_data("reference targets a missing image attachment"));
        }
        self.add_reference(container, ReferenceTarget::Image(image))
    }

    pub fn remove_reference(
        &mut self,
        container: &ContainerId,
        reference: &ReferenceId,
    ) -> io::Result<()> {
        let members = &mut self
            .containers
            .get_mut(container)
            .ok_or_else(|| invalid_data("container is missing"))?
            .members;
        members.retain(|member| &member.id != reference);
        Ok(())
    }

    pub fn add_snippet_to_root(&mut self, entity: EntityId) -> ReferenceId {
        let root = self.root.clone();
        self.add_snippet_reference(&root, entity)
            .expect("workspace root container must exist")
    }

    pub fn add_container_to_root(&mut self, container: ContainerId) -> ReferenceId {
        let root = self.root.clone();
        self.add_container_reference(&root, container)
            .expect("workspace root container must exist")
    }

    pub fn add_special_reference(
        &mut self,
        container: &ContainerId,
        kind: SpecialKind,
    ) -> io::Result<ReferenceId> {
        self.add_reference(container, ReferenceTarget::Special(kind))
    }

    pub fn add_special_to_root(&mut self, kind: SpecialKind) -> ReferenceId {
        let root = self.root.clone();
        self.add_special_reference(&root, kind)
            .expect("workspace root container must exist")
    }

    /// Ensures `container` holds exactly one reference to `kind`; returns
    /// whether a reference was added.
    pub fn ensure_special(
        &mut self,
        container: &ContainerId,
        kind: SpecialKind,
    ) -> io::Result<bool> {
        let exists = self.containers.get(container).is_some_and(|container| {
            container
                .members
                .iter()
                .any(|reference| reference.target == ReferenceTarget::Special(kind))
        });
        if exists {
            Ok(false)
        } else {
            self.add_special_reference(container, kind)?;
            Ok(true)
        }
    }

    fn add_reference(
        &mut self,
        container: &ContainerId,
        target: ReferenceTarget,
    ) -> io::Result<ReferenceId> {
        self.add_reference_with_role(container, target, MemberRole::Normal)
    }

    fn add_reference_with_role(
        &mut self,
        container: &ContainerId,
        target: ReferenceTarget,
        role: MemberRole,
    ) -> io::Result<ReferenceId> {
        let id = ReferenceId::new();
        self.containers
            .get_mut(container)
            .ok_or_else(|| invalid_data("container is missing"))?
            .members
            .push(Reference {
                id: id.clone(),
                target,
                alias: None,
                presentation: ReferencePresentation::Card,
                role,
            });
        Ok(id)
    }

    /// Removes every edge to an entity. Containers are independent graph nodes and
    /// deliberately have no parent pointer.
    pub fn remove_entity_references(&mut self, entity: &EntityId) {
        for container in self.containers.values_mut() {
            container.members.retain(|reference| {
                !matches!(&reference.target, ReferenceTarget::Snippet(id) if id == entity)
            });
        }
    }

    fn validate(&self) -> io::Result<()> {
        if self.version != WORKSPACE_VERSION {
            return Err(invalid_data("unsupported workspace version"));
        }
        if !self.containers.contains_key(&self.root) {
            return Err(invalid_data("workspace root container is missing"));
        }
        let mut reference_ids = BTreeSet::new();
        for (id, container) in &self.containers {
            if id != &container.id {
                return Err(invalid_data("container map key does not match its id"));
            }
            for reference in &container.members {
                if !reference_ids.insert(reference.id.clone()) {
                    return Err(invalid_data("duplicate reference id"));
                }
                if let ReferenceTarget::Container(target) = &reference.target
                    && !self.containers.contains_key(target)
                {
                    return Err(invalid_data("reference targets a missing container"));
                }
                if let ReferenceTarget::Image(id) = &reference.target
                    && !self.images.contains_key(id)
                {
                    return Err(invalid_data("reference targets a missing image attachment"));
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CardLayout {
    pub position: [f32; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 4]>,
    /// Display size in logical points. For image references this is the image
    /// rectangle; for other references the field is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<[f32; 2]>,
    /// How the image is fitted inside the display rectangle. Only meaningful
    /// for `ReferenceTarget::Image` references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_fit: Option<ImageFit>,
}

/// A canvas-local text annotation. Texts are not part of the entity-reference
/// graph: they cannot be referenced, linked, or moved across container
/// boundaries. Persisted with the per-container `ContainerLayout`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CanvasText {
    pub id: TextId,
    pub position: [f32; 2],
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 4]>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerLayout {
    pub version: u32,
    pub container: ContainerId,
    #[serde(default)]
    pub items: BTreeMap<ReferenceId, CardLayout>,
    #[serde(default)]
    pub texts: Vec<CanvasText>,
}

impl ContainerLayout {
    pub fn empty(container: ContainerId) -> Self {
        Self {
            version: LAYOUT_VERSION,
            container,
            items: BTreeMap::new(),
            texts: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct WorkspaceStore {
    root: PathBuf,
}

impl WorkspaceStore {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join(".floatdea/layout"))?;
        Ok(Self { root })
    }

    pub fn load_or_initialize(&self, snippets: &[Snippet]) -> io::Result<Workspace> {
        let path = self.root.join("workspace.json");
        let mut workspace = if path.exists() {
            let workspace: Workspace = read_json(&path)?;
            workspace.validate()?;
            workspace
        } else {
            let mut workspace = Workspace::empty();
            for snippet in snippets {
                workspace.add_snippet_to_root(snippet.id.clone());
            }
            workspace
        };
        // Ensure the permanent settings entry exists in the root box (migrates
        // workspaces created before special items were introduced).
        let root = workspace.root.clone();
        let added = workspace.ensure_special(&root, SpecialKind::Settings)?;
        if added || !path.exists() {
            self.save(&workspace)?;
        }
        Ok(workspace)
    }

    pub fn save(&self, workspace: &Workspace) -> io::Result<()> {
        workspace.validate()?;
        write_json_atomic(&self.root.join("workspace.json"), workspace)
    }

    pub fn load_layout(&self, container: &ContainerId) -> io::Result<ContainerLayout> {
        let path = self.layout_path(container);
        if !path.exists() {
            return Ok(ContainerLayout::empty(container.clone()));
        }
        let layout: ContainerLayout = read_json(&path)?;
        if layout.version != LAYOUT_VERSION || &layout.container != container {
            return Err(invalid_data("layout does not match its container"));
        }
        Ok(layout)
    }

    pub fn save_layout(&self, layout: &ContainerLayout) -> io::Result<()> {
        if layout.version != LAYOUT_VERSION {
            return Err(invalid_data("unsupported layout version"));
        }
        write_json_atomic(&self.layout_path(&layout.container), layout)
    }

    fn layout_path(&self, container: &ContainerId) -> PathBuf {
        self.root
            .join(".floatdea/layout")
            .join(format!("{}.json", container.as_str()))
    }
}

fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<T> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(invalid_data)
}

fn write_json_atomic(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(invalid_data)?;
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}

fn invalid_data(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TestFolder(PathBuf);

    impl TestFolder {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "floatdea-workspace-store-{}-{nonce}",
                std::process::id()
            )))
        }
    }

    impl Drop for TestFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn initializes_root_with_references_not_owned_entities() {
        let folder = TestFolder::new();
        let store = WorkspaceStore::open(&folder.0).unwrap();
        let snippet = Snippet {
            id: EntityId::new(),
            title: "hello".to_owned(),
            content: String::new(),
        };

        let workspace = store
            .load_or_initialize(std::slice::from_ref(&snippet))
            .unwrap();

        assert!(matches!(
            &workspace.root().members[0].target,
            ReferenceTarget::Snippet(id) if id == &snippet.id
        ));
        assert!(folder.0.join("workspace.json").is_file());
    }

    #[test]
    fn saves_each_container_layout_separately() {
        let folder = TestFolder::new();
        let store = WorkspaceStore::open(&folder.0).unwrap();
        let container = ContainerId::new();
        let reference = ReferenceId::new();
        let mut layout = ContainerLayout::empty(container.clone());
        layout.items.insert(
            reference.clone(),
            CardLayout {
                position: [41.0, 73.0],
                color: None,
                size: None,
                image_fit: None,
            },
        );

        store.save_layout(&layout).unwrap();

        assert_eq!(
            store.load_layout(&container).unwrap().items[&reference].position,
            [41.0, 73.0]
        );
    }

    #[test]
    fn persists_canvas_texts_in_container_layout() {
        let folder = TestFolder::new();
        let store = WorkspaceStore::open(&folder.0).unwrap();
        let container = ContainerId::new();
        let mut layout = ContainerLayout::empty(container.clone());
        layout.texts.push(CanvasText {
            id: TextId::new(),
            position: [12.0, 34.0],
            text: "plain text".to_owned(),
            color: None,
        });

        store.save_layout(&layout).unwrap();

        let loaded = store.load_layout(&container).unwrap();
        assert_eq!(loaded.texts.len(), 1);
        assert_eq!(loaded.texts[0].position, [12.0, 34.0]);
        assert_eq!(loaded.texts[0].text, "plain text");
    }

    #[test]
    fn loads_legacy_layout_without_texts() {
        let folder = TestFolder::new();
        let store = WorkspaceStore::open(&folder.0).unwrap();
        let container = ContainerId::new();
        let path = store.layout_path(&container);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "{{\"version\":1,\"container\":\"{}\",\"items\":{{}}}}",
                container.as_str()
            ),
        )
        .unwrap();

        let loaded = store.load_layout(&container).unwrap();

        assert!(loaded.texts.is_empty());
    }

    #[test]
    fn adds_container_reference_to_root() {
        let mut workspace = Workspace::empty();
        let container = workspace.create_container("Folder");
        let reference = workspace.add_container_to_root(container.clone());

        assert!(matches!(
            &workspace.root().members[0].target,
            ReferenceTarget::Container(id) if id == &container
        ));
        assert_eq!(workspace.root().members[0].id, reference);
    }

    #[test]
    fn root_holds_the_settings_special_after_initialization() {
        let folder = TestFolder::new();
        let store = WorkspaceStore::open(&folder.0).unwrap();
        let snippet = Snippet {
            id: EntityId::new(),
            title: "hello".to_owned(),
            content: String::new(),
        };

        let workspace = store
            .load_or_initialize(std::slice::from_ref(&snippet))
            .unwrap();

        assert!(workspace.root().members.iter().any(|reference| {
            matches!(
                &reference.target,
                ReferenceTarget::Special(SpecialKind::Settings)
            )
        }));
    }

    #[test]
    fn ensure_special_is_idempotent() {
        let mut workspace = Workspace::empty();
        let root = workspace.root.clone();
        assert!(
            workspace
                .ensure_special(&root, SpecialKind::Settings)
                .unwrap()
        );
        assert!(
            !workspace
                .ensure_special(&root, SpecialKind::Settings)
                .unwrap()
        );
        assert_eq!(
            workspace
                .root()
                .members
                .iter()
                .filter(|reference| matches!(
                    &reference.target,
                    ReferenceTarget::Special(SpecialKind::Settings)
                ))
                .count(),
            1
        );
    }

    #[test]
    fn containers_form_a_graph_without_parent_ownership() {
        let mut workspace = Workspace::empty();
        let left = workspace.create_container("Left");
        let right = workspace.create_container("Right");
        let entity = EntityId::new();

        workspace
            .add_snippet_reference(&left, entity.clone())
            .unwrap();
        workspace
            .add_snippet_reference(&right, entity.clone())
            .unwrap();
        workspace
            .add_container_reference(&left, right.clone())
            .unwrap();
        workspace
            .add_container_reference(&right, left.clone())
            .unwrap();

        assert!(workspace.validate().is_ok());
        assert_eq!(workspace.containers[&left].members.len(), 2);
        assert_eq!(workspace.containers[&right].members.len(), 2);
    }

    #[test]
    fn ai_boxes_are_containers_with_ai_workspace_kind() {
        let mut workspace = Workspace::empty();
        let ai_box = workspace.create_ai_box("Analysis");
        assert!(workspace.is_ai_box(&ai_box));
        assert_eq!(
            workspace.containers[&ai_box].kind,
            ContainerKind::AiWorkspace
        );
        let _ = workspace.add_container_to_root(ai_box.clone());
        assert!(workspace.validate().is_ok());
        // Ordinary containers stay Normal.
        let folder = workspace.create_container("Folder");
        assert_eq!(workspace.containers[&folder].kind, ContainerKind::Normal);
        assert!(!workspace.is_ai_box(&folder));
    }

    #[test]
    fn ai_box_member_roles_persist_through_the_store() {
        let folder = TestFolder::new();
        let store = WorkspaceStore::open(&folder.0).unwrap();
        let mut workspace = Workspace::empty();
        let ai_box = workspace.create_ai_box("AI");
        let entity = EntityId::new();
        let conversation = ConversationId::new();
        let _ = workspace
            .add_source_reference(&ai_box, ReferenceTarget::Snippet(entity.clone()))
            .unwrap();
        let conversation_ref = workspace
            .add_conversation_card(&ai_box, conversation.clone())
            .unwrap();
        let _ = workspace
            .add_output_reference(&ai_box, entity.clone())
            .unwrap();
        let _ = workspace
            .add_snippet_reference(&ai_box, entity.clone())
            .unwrap();
        let _ = workspace.add_container_to_root(ai_box.clone());
        store.save(&workspace).unwrap();

        let loaded = store.load_or_initialize(&[]).unwrap();
        let members = &loaded.containers[&ai_box].members;
        let source = members
            .iter()
            .find(|reference| reference.role == MemberRole::Source)
            .expect("source role");
        assert!(matches!(
            &source.target,
            ReferenceTarget::Snippet(id) if id == &entity
        ));
        let output = members
            .iter()
            .find(|reference| reference.role == MemberRole::Output)
            .expect("output role");
        assert!(matches!(
            &output.target,
            ReferenceTarget::Snippet(id) if id == &entity
        ));
        let conversation_member = members
            .iter()
            .find(|reference| reference.id == conversation_ref)
            .expect("conversation card");
        assert!(matches!(
            &conversation_member.target,
            ReferenceTarget::Conversation(id) if id == &conversation
        ));
        assert_eq!(conversation_member.role, MemberRole::Conversation);
        // A plain snippet reference inside the AI box keeps Normal role.
        assert!(
            members
                .iter()
                .any(|reference| reference.role == MemberRole::Normal)
        );
    }

    #[test]
    fn conversation_cards_validate_without_sidecar_entries() {
        let mut workspace = Workspace::empty();
        let ai_box = workspace.create_ai_box("AI");
        let _ = workspace
            .add_conversation_card(&ai_box, ConversationId::new())
            .unwrap();
        // The sidecar store is separate from the workspace graph, so a
        // conversation card never fails workspace validation.
        assert!(workspace.validate().is_ok());
    }

    #[test]
    fn remove_entity_references_leaves_conversation_cards() {
        let mut workspace = Workspace::empty();
        let ai_box = workspace.create_ai_box("AI");
        let entity = EntityId::new();
        let _ = workspace
            .add_source_reference(&ai_box, ReferenceTarget::Snippet(entity.clone()))
            .unwrap();
        let _ = workspace
            .add_conversation_card(&ai_box, ConversationId::new())
            .unwrap();
        workspace.remove_entity_references(&entity);

        let members = &workspace.containers[&ai_box].members;
        assert!(!members.iter().any(|reference| matches!(
            &reference.target,
            ReferenceTarget::Snippet(id) if id == &entity
        )));
        assert!(members
            .iter()
            .any(|reference| matches!(reference.target, ReferenceTarget::Conversation(_))));
    }
}
