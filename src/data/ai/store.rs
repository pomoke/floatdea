//! File-backed storage for AI sidecar state. Each AI box gets its own file
//! under `.floatdea/ai/{container_id}.json`, so deleting or duplicating one AI
//! box never affects another box's conversations.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Serialize, de::DeserializeOwned};

use super::{AiBoxData, AI_BOX_DATA_VERSION};
use crate::data::ContainerId;

/// Persists per-AI-box sidecar data (conversations, messages, snapshots).
/// This data is local to the workspace and is intentionally kept separate from
/// the Markdown entities and the workspace graph.
#[derive(Clone, Debug)]
pub struct AiStore {
    root: PathBuf,
}

impl AiStore {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join(".floatdea/ai"))?;
        Ok(Self { root })
    }

    /// Loads the sidecar data for one AI box. Missing or incompatible files
    /// degrade to an empty box so a corrupt sidecar never blocks the workspace.
    pub fn load_box(&self, ai_box: &ContainerId) -> AiBoxData {
        let path = self.box_path(ai_box);
        match read_json::<AiBoxData>(&path) {
            Ok(data) if data.version == AI_BOX_DATA_VERSION && &data.ai_box == ai_box => data,
            _ => AiBoxData::empty(ai_box.clone()),
        }
    }

    pub fn save_box(&self, data: &AiBoxData) -> io::Result<()> {
        write_json_atomic(&self.box_path(&data.ai_box), data)
    }

    /// Deletes the sidecar file of an AI box (conversations are gone with it).
    /// Missing files are not an error.
    pub fn remove_box(&self, ai_box: &ContainerId) -> io::Result<()> {
        match fs::remove_file(self.box_path(ai_box)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn box_path(&self, ai_box: &ContainerId) -> PathBuf {
        self.root
            .join(".floatdea/ai")
            .join(format!("{}.json", ai_box.as_str()))
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
    use crate::data::{ConversationId, EntityId, TurnTaskId};
    use crate::data::ai::{Message, MessageRole, SourceTarget};

    struct TestFolder(PathBuf);

    impl TestFolder {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            Self(std::env::temp_dir().join(format!(
                "floatdea-ai-store-{}-{nonce}",
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
    fn missing_box_loads_as_empty() {
        let folder = TestFolder::new();
        let store = AiStore::open(&folder.0).unwrap();
        let ai_box = ContainerId::new();
        let data = store.load_box(&ai_box);
        assert_eq!(data.ai_box, ai_box);
        assert!(data.conversations.is_empty());
    }

    #[test]
    fn persists_conversations_across_reloads() {
        let folder = TestFolder::new();
        let store = AiStore::open(&folder.0).unwrap();
        let ai_box = ContainerId::new();
        let conversation = ConversationId::new();
        let entity = EntityId::new();

        let mut data = AiBoxData::empty(ai_box.clone());
        assert!(data.create_conversation(
            conversation.clone(),
            "Ask",
            false,
            vec![SourceTarget::Snippet(entity)],
            1,
        ));
        assert!(data.push_message(&conversation, Message::user("hello"), 2));
        let task = TurnTaskId::new();
        assert!(data.push_message(
            &conversation,
            Message::assistant("hi", task.clone()),
            3,
        ));
        store.save_box(&data).unwrap();

        let loaded = store.load_box(&ai_box);
        let loaded_conversation = loaded.get(&conversation).expect("conversation survives");
        assert_eq!(loaded_conversation.title, "Ask");
        assert!(!loaded_conversation.temporary);
        assert_eq!(loaded_conversation.messages.len(), 2);
        assert_eq!(loaded_conversation.messages[1].role, MessageRole::Assistant);
        assert_eq!(
            loaded_conversation.messages[1].turn_task.as_ref(),
            Some(&task)
        );
        assert!(matches!(
            loaded_conversation.sources[0],
            SourceTarget::Snippet(_)
        ));
    }

    #[test]
    fn deleting_a_box_removes_only_its_sidecar() {
        let folder = TestFolder::new();
        let store = AiStore::open(&folder.0).unwrap();
        let first = ContainerId::new();
        let second = ContainerId::new();
        let mut data = AiBoxData::empty(first.clone());
        assert!(data.create_conversation(ConversationId::new(), "C", false, Vec::new(), 1));
        store.save_box(&data).unwrap();
        let mut other = AiBoxData::empty(second.clone());
        assert!(other.create_conversation(ConversationId::new(), "D", true, Vec::new(), 1));
        store.save_box(&other).unwrap();

        store.remove_box(&first).unwrap();

        assert!(store.load_box(&first).conversations.is_empty());
        assert_eq!(store.load_box(&second).conversations.len(), 1);
    }

    #[test]
    fn incompatible_sidecar_degrades_to_empty() {
        let folder = TestFolder::new();
        let store = AiStore::open(&folder.0).unwrap();
        let ai_box = ContainerId::new();
        let path = store.box_path(&ai_box);
        fs::write(&path, "{\"version\":99,\"ai_box\":\"bad\",\"conversations\":{}}").unwrap();

        let loaded = store.load_box(&ai_box);
        assert_eq!(loaded.version, AI_BOX_DATA_VERSION);
        assert_eq!(loaded.ai_box, ai_box);
        assert!(loaded.conversations.is_empty());
    }

    #[test]
    fn temporary_flag_survives_round_trip() {
        let folder = TestFolder::new();
        let store = AiStore::open(&folder.0).unwrap();
        let ai_box = ContainerId::new();
        let conversation = ConversationId::new();
        let mut data = AiBoxData::empty(ai_box.clone());
        assert!(data.create_conversation(conversation.clone(), "Temp", true, Vec::new(), 0));
        store.save_box(&data).unwrap();

        assert!(store.load_box(&ai_box).get(&conversation).unwrap().temporary);
    }
}
