//! Sidecar state for AI workspaces: conversations, messages and per-turn
//! source snapshots. This state is local to each AI box, never becomes a
//! Markdown entity and is not part of the normal entity-reference graph.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::data::{ContainerId, ConversationId, EntityId, TurnTaskId};

/// The stable version of the per-AI-box sidecar file.
pub const AI_BOX_DATA_VERSION: u32 = 1;

/// A stable reference to a source entity that can be bound to a conversation
/// or recorded as actually used by a turn. Only stable ids are stored; titles
/// and content are captured separately so identity never depends on a title.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum SourceTarget {
    Snippet(EntityId),
    Container(ContainerId),
}

impl SourceTarget {
    /// Stable identity key used for de-duplication across multiple references
    /// to the same entity (the plan de-duplicates by `EntityId`).
    pub fn stable_key(&self) -> String {
        match self {
            SourceTarget::Snippet(id) => format!("s:{}", id.as_str()),
            SourceTarget::Container(id) => format!("c:{}", id.as_str()),
        }
    }
}

/// A source actually used by one assistant turn: the stable target plus the
/// title and content hash captured at send time (a runtime snapshot, not a
/// copy of the source).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    pub target: SourceTarget,
    /// Human-readable title captured at send time (display only).
    #[serde(default)]
    pub title: String,
    /// Hash of the source content actually sent for this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

/// Role of a single conversation message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

/// Lifecycle status of a turn (only meaningful on assistant messages).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatus {
    /// The answer completed normally.
    #[default]
    Completed,
    /// Generation was stopped by the user; only a partial answer exists.
    Stopped,
    /// The turn failed (provider error, timeout, offline, …).
    Failed,
    /// A bound source changed after this answer was generated, so the answer
    /// is based on an older snapshot.
    Stale,
}

/// A single conversation message (user question or assistant answer).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: MessageRole,
    pub content: String,
    /// Sources actually used by the turn that produced this message (empty for
    /// user messages).
    #[serde(default)]
    pub sources: Vec<SourceRef>,
    /// Turn lifecycle status.
    #[serde(default)]
    pub status: MessageStatus,
    /// The turn task identity for assistant messages (used to match streaming
    /// events and cancellation).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_task: Option<TurnTaskId>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: content.into(),
            sources: Vec::new(),
            status: MessageStatus::Completed,
            turn_task: None,
        }
    }

    pub fn assistant(content: impl Into<String>, turn_task: TurnTaskId) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: content.into(),
            sources: Vec::new(),
            status: MessageStatus::Completed,
            turn_task: Some(turn_task),
        }
    }
}

/// A conversation inside an AI box. Conversations are sidecar state: they do
/// not get an `EntityId`, are not referenced by ordinary boxes and do not enter
/// search, indexing or export by default. Only "Save as Snippet" answers become
/// real Markdown entities.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: ConversationId,
    pub title: String,
    /// Temporary conversations are cleared on close and never persisted;
    /// ordinary conversations are saved locally by default.
    #[serde(default)]
    pub temporary: bool,
    /// Stable ids of the sources bound to this conversation. Adding/removing
    /// only affects future turns; past answers keep their own snapshots.
    #[serde(default)]
    pub sources: Vec<SourceTarget>,
    /// Ordered message history (user and assistant interleaved).
    #[serde(default)]
    pub messages: Vec<Message>,
    /// Creation time (seconds since Unix epoch).
    pub created_at: u64,
    /// Last activity time (seconds since Unix epoch).
    #[serde(default)]
    pub updated_at: u64,
    /// Provider/model label used for display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Conversation {
    pub fn new(
        id: ConversationId,
        title: impl Into<String>,
        temporary: bool,
        sources: Vec<SourceTarget>,
        now: u64,
    ) -> Self {
        Self {
            id,
            title: title.into(),
            temporary,
            sources,
            messages: Vec::new(),
            created_at: now,
            updated_at: now,
            model: None,
        }
    }
}

/// All sidecar state for one AI box, keyed by its `ContainerId`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AiBoxData {
    pub version: u32,
    pub ai_box: ContainerId,
    #[serde(default)]
    pub conversations: BTreeMap<ConversationId, Conversation>,
}

impl AiBoxData {
    pub fn empty(ai_box: ContainerId) -> Self {
        Self {
            version: AI_BOX_DATA_VERSION,
            ai_box,
            conversations: BTreeMap::new(),
        }
    }

    pub fn get(&self, id: &ConversationId) -> Option<&Conversation> {
        self.conversations.get(id)
    }

    pub fn get_mut(&mut self, id: &ConversationId) -> Option<&mut Conversation> {
        self.conversations.get_mut(id)
    }

    /// Creates a conversation and returns whether it was inserted (false when
    /// a conversation with the same id already exists).
    pub fn create_conversation(
        &mut self,
        id: ConversationId,
        title: impl Into<String>,
        temporary: bool,
        sources: Vec<SourceTarget>,
        now: u64,
    ) -> bool {
        if self.conversations.contains_key(&id) {
            return false;
        }
        let sources = dedup_sources(sources);
        self.conversations.insert(
            id.clone(),
            Conversation::new(id, title, temporary, sources, now),
        );
        true
    }

    /// Deletes a conversation (messages, title and conversation-level source
    /// bindings). Returns whether it existed. Source entities, saved outputs
    /// and source entities themselves are never touched.
    pub fn delete_conversation(&mut self, id: &ConversationId) -> bool {
        self.conversations.remove(id).is_some()
    }

    pub fn rename_conversation(&mut self, id: &ConversationId, title: impl Into<String>) -> bool {
        match self.conversations.get_mut(id) {
            Some(conversation) => {
                conversation.title = title.into();
                true
            }
            None => false,
        }
    }

    /// Binds a source to the conversation, de-duplicated by stable id.
    pub fn bind_source(&mut self, id: &ConversationId, target: SourceTarget) -> bool {
        let Some(conversation) = self.conversations.get_mut(id) else {
            return false;
        };
        if conversation.sources.iter().any(|existing| existing.stable_key() == target.stable_key()) {
            return false;
        }
        conversation.sources.push(target);
        true
    }

    /// Unbinds a source from the conversation; only affects future turns.
    pub fn unbind_source(&mut self, id: &ConversationId, target: &SourceTarget) -> bool {
        let Some(conversation) = self.conversations.get_mut(id) else {
            return false;
        };
        let before = conversation.sources.len();
        conversation
            .sources
            .retain(|existing| existing.stable_key() != target.stable_key());
        before != conversation.sources.len()
    }

    /// Appends a message and bumps the last-activity timestamp.
    pub fn push_message(&mut self, id: &ConversationId, message: Message, now: u64) -> bool {
        match self.conversations.get_mut(id) {
            Some(conversation) => {
                conversation.messages.push(message);
                conversation.updated_at = now;
                true
            }
            None => false,
        }
    }
}

/// De-duplicates a source list by stable id, preserving first-seen order.
pub(crate) fn dedup_sources(sources: Vec<SourceTarget>) -> Vec<SourceTarget> {
    let mut seen = std::collections::BTreeSet::new();
    sources
        .into_iter()
        .filter(|source| seen.insert(source.stable_key()))
        .collect()
}

/// Stable, deterministic FNV-1a 64-bit content hash (hex). Used to detect
/// source changes between turns; not cryptographic.
pub fn content_hash(content: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Seconds since the Unix epoch, used for conversation timestamps.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation_id() -> ConversationId {
        ConversationId::new()
    }

    #[test]
    fn content_hash_is_stable_and_sensitive_to_content() {
        let a = content_hash("same content");
        assert_eq!(a, content_hash("same content"));
        assert_ne!(a, content_hash("different content"));
        assert_ne!(content_hash(""), a);
    }

    #[test]
    fn conversation_lifecycle_mutates_sidecar_data() {
        let ai_box = ContainerId::new();
        let mut data = AiBoxData::empty(ai_box);
        let id = conversation_id();
        let entity = EntityId::new();
        let source = SourceTarget::Snippet(entity);

        assert!(data.create_conversation(id.clone(), "Ask", false, vec![source.clone()], 1));
        // Duplicate id is rejected.
        assert!(!data.create_conversation(id.clone(), "Again", false, Vec::new(), 1));

        // Binding is de-duplicated by stable id.
        assert!(!data.bind_source(&id, source.clone()));
        assert!(data.bind_source(&id, SourceTarget::Container(ContainerId::new())));
        assert_eq!(data.get(&id).unwrap().sources.len(), 2);

        let task = TurnTaskId::new();
        assert!(data.push_message(&id, Message::user("hello"), 2));
        assert!(data.push_message(&id, Message::assistant("hi there", task.clone()), 3));
        assert_eq!(data.get(&id).unwrap().updated_at, 3);
        assert_eq!(data.get(&id).unwrap().messages.len(), 2);
        assert_eq!(data.get(&id).unwrap().messages[1].turn_task.as_ref(), Some(&task));

        assert!(data.rename_conversation(&id, "Renamed"));
        assert_eq!(data.get(&id).unwrap().title, "Renamed");

        assert!(data.delete_conversation(&id));
        assert!(data.get(&id).is_none());
        assert!(!data.delete_conversation(&id));
    }

    #[test]
    fn unbinding_source_only_affects_future_turns() {
        let mut data = AiBoxData::empty(ContainerId::new());
        let id = conversation_id();
        let entity = EntityId::new();
        let source = SourceTarget::Snippet(entity);
        data.create_conversation(id.clone(), "C", false, vec![source.clone()], 0);
        assert!(data.unbind_source(&id, &source));
        assert!(data.get(&id).unwrap().sources.is_empty());
        // Unbinding an unknown source is a no-op.
        assert!(!data.unbind_source(&id, &SourceTarget::Snippet(EntityId::new())));
    }

    #[test]
    fn sources_deduplicate_by_stable_key_on_creation() {
        let entity = EntityId::new();
        let sources = vec![
            SourceTarget::Snippet(entity.clone()),
            SourceTarget::Snippet(entity),
        ];
        let deduped = dedup_sources(sources);
        assert_eq!(deduped.len(), 1);
    }
}
