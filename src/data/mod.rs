//! UI-independent snippet data and local file storage.

pub mod ai;
mod model;
pub mod settings;
pub mod storage;
pub mod workspace;

pub use model::{
    ContainerId, ConversationId, EntityId, ExternalFileId, ReferenceId, Snippet, TextId, TurnTaskId,
};
pub use settings::{Settings, SettingsStore, ThemeSetting};
pub use workspace::{CanvasText, ContainerKind, ExternalFileRef, MemberRole};
