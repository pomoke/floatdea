//! UI-independent snippet data and local file storage.

pub mod ai;
pub mod attachment;
pub mod markdown_targets;
mod model;
pub mod settings;
pub mod storage;
pub mod workspace;

pub use markdown_targets::{
    LocalMarkdownTarget, ParsedMarkdownTarget, parse_attachment_link, parse_local_markdown_targets,
    parse_snippet_link,
};
pub use model::{
    AttachmentId, ContainerId, ConversationId, EntityId, ExternalFileId, ReferenceId, Snippet,
    TextId, TurnTaskId,
};
pub use settings::{Settings, SettingsStore, ThemeSetting};
pub use workspace::{
    CanvasText, ContainerKind, ExternalFileRef, ImageAttachment, ImageFit, MemberRole,
};
