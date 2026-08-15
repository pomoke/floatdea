//! AI sidecar state for special AI boxes.
//!
//! Conversations, messages and per-turn source snapshots are local sidecar
//! data partitioned by AI box `ContainerId`. They never become Markdown
//! entities, never enter the ordinary entity-reference graph, and are never
//! synced or exported by default. Only answers the user saves as snippets
//! become real knowledge entities.

pub mod provider;
mod store;
mod types;
pub mod worker;

pub use provider::{
    AiError, AiErrorKind, CancelFlag, Capabilities, ChatMessage, ChatProvider, ChatRequest,
    ChatRole, ProviderConfig, ProviderKind, StreamEvent, StreamOutcome, TokenUsage, build_provider,
};
pub use store::AiStore;
pub use types::{
    AI_BOX_DATA_VERSION, AiBoxData, Conversation, Message, MessageRole, MessageStatus, SourceRef,
    SourceTarget, content_hash, now_unix,
};
pub use worker::{AiWorker, TurnEvent, TurnIdentity, TurnRequest};
