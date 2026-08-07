//! UI-independent snippet data and local file storage.

mod model;
pub mod settings;
pub mod storage;
pub mod workspace;

pub use model::{ContainerId, EntityId, ReferenceId, Snippet, TextId};
pub use settings::{Settings, SettingsStore, ThemeSetting};
pub use workspace::CanvasText;
