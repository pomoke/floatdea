//! UI-independent snippet data and local file storage.

mod model;
pub mod storage;
pub mod workspace;

pub use model::{ContainerId, EntityId, ReferenceId, Snippet, TextId};
pub use workspace::CanvasText;
