use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{ContainerId, EntityId, ReferenceId, Snippet, TextId};

const WORKSPACE_VERSION: u32 = 1;
const LAYOUT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum ReferenceTarget {
    Snippet(EntityId),
    Container(ContainerId),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    pub id: ReferenceId,
    pub target: ReferenceTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(default)]
    pub presentation: ReferencePresentation,
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
    pub members: Vec<Reference>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub version: u32,
    pub root: ContainerId,
    pub containers: BTreeMap<ContainerId, Container>,
}

impl Workspace {
    pub fn empty() -> Self {
        let root = ContainerId::new();
        let container = Container {
            id: root.clone(),
            title: "Home".to_owned(),
            members: Vec::new(),
        };
        Self {
            version: WORKSPACE_VERSION,
            root: root.clone(),
            containers: BTreeMap::from([(root, container)]),
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
                members: Vec::new(),
            },
        );
        id
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

    fn add_reference(
        &mut self,
        container: &ContainerId,
        target: ReferenceTarget,
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
        if path.exists() {
            let workspace: Workspace = read_json(&path)?;
            workspace.validate()?;
            return Ok(workspace);
        }

        let mut workspace = Workspace::empty();
        for snippet in snippets {
            workspace.add_snippet_to_root(snippet.id.clone());
        }
        self.save(&workspace)?;
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
}
