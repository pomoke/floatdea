//! User settings persisted to `.floatdea/settings.json`. Settings are local to
//! the workspace and survive restarts; the UI lives in the system "设置"
//! (Settings) special item.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::data::ai::provider::ProviderKind;
use crate::data::ai::ProviderConfig;

const SETTINGS_VERSION: u32 = 1;

/// Which color theme the app uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeSetting {
    /// Follow the operating system theme.
    #[default]
    System,
    Light,
    Dark,
}

/// How snippet and folder windows are presented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowMode {
    /// Each canvas and snippet opens in its own native OS window
    /// (egui multi-viewport).
    #[default]
    Native,
    /// Everything floats as freely draggable windows inside the single main
    /// window (full-window mode).
    Floating,
}

/// Application settings. New fields must carry a `#[serde(default)]` so older
/// settings files keep loading.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub version: u32,
    pub theme: ThemeSetting,
    /// Font size (points) of the markdown preview body text; headings derive
    /// from it.
    #[serde(default = "default_preview_font_size")]
    pub preview_font_size: f32,
    /// Height cap of rendered math formulas, as a multiple of one body-text
    /// line (MathJax SVG metrics render larger than egui text).
    #[serde(default = "default_math_cap_scale")]
    pub math_cap_scale: f32,
    /// Snap dragged cards to the 32 pt canvas grid.
    #[serde(default = "default_snap_to_grid")]
    pub snap_to_grid: bool,
    /// Draw the 32 pt dot grid on every canvas.
    #[serde(default = "default_show_grid")]
    pub show_grid: bool,
    /// How snippet/folder windows are presented: native OS windows or floating
    /// windows inside the main window. Missing files fall back to `Native`.
    #[serde(default)]
    pub window_mode: WindowMode,
    /// Master switch for AI. AI is **off by default**: while disabled the app
    /// never issues a model network request, but AI boxes remain usable as
    /// read-only workbenches.
    #[serde(default)]
    pub ai_enabled: bool,
    /// Which provider family conversations use.
    #[serde(default)]
    pub ai_provider: ProviderKind,
    /// Model name as shown to the user (e.g. "gpt-4o-mini", "llama3.2").
    #[serde(default = "default_ai_model")]
    pub ai_model: String,
    /// Optional custom base URL for OpenAI-compatible endpoints / Ollama.
    #[serde(default)]
    pub ai_base_url: String,
    /// The API key value. Stored directly in `.floatdea/settings.json` for
    /// usability (plan_ai.md §10 prefers an OS credential store; the keychain
    /// integration is future work). The UI masks it as a password field and it
    /// is never written to logs, markdown or audit records.
    #[serde(default)]
    pub ai_api_key: String,
    /// Whether the model may call the bounded built-in tools
    /// (`core.list_sources` / `read_source` / `search_sources` /
    /// `create_output_proposal`, plan_ai.md §9.8). When off, conversations are
    /// plain chat and no tool receipts or proposals are produced.
    #[serde(default = "default_ai_tools_enabled")]
    pub ai_tools_enabled: bool,
    /// Optional model name for lightweight auxiliary tasks (title generation,
    /// summarization, etc.). When empty the main `ai_model` is used instead.
    /// Uses the same provider, base URL and API key as the main model.
    #[serde(default)]
    pub summarizer_model: String,
}

impl Settings {
    /// Builds the runtime provider configuration for the current AI settings.
    pub fn ai_provider_config(&self) -> ProviderConfig {
        ProviderConfig {
            kind: self.ai_provider,
            model: self.ai_model.clone(),
            base_url: (!self.ai_base_url.trim().is_empty())
                .then(|| self.ai_base_url.trim().to_owned()),
            api_key: (!self.ai_api_key.trim().is_empty())
                .then(|| self.ai_api_key.trim().to_owned()),
        }
    }

    /// Provider config for the summarizer model (used for lightweight tasks
    /// such as auto-generating conversation titles). Falls back to the main
    /// conversation model when `summarizer_model` is blank.
    pub fn summarizer_provider_config(&self) -> ProviderConfig {
        let model = self.summarizer_model.trim();
        if model.is_empty() {
            return self.ai_provider_config();
        }
        ProviderConfig {
            kind: self.ai_provider,
            model: model.to_owned(),
            base_url: (!self.ai_base_url.trim().is_empty())
                .then(|| self.ai_base_url.trim().to_owned()),
            api_key: (!self.ai_api_key.trim().is_empty())
                .then(|| self.ai_api_key.trim().to_owned()),
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            theme: ThemeSetting::default(),
            preview_font_size: default_preview_font_size(),
            math_cap_scale: default_math_cap_scale(),
            snap_to_grid: default_snap_to_grid(),
            show_grid: default_show_grid(),
            window_mode: WindowMode::default(),
            ai_enabled: false,
            ai_provider: ProviderKind::Fake,
            ai_model: default_ai_model(),
            ai_base_url: String::new(),
            ai_api_key: String::new(),
            ai_tools_enabled: default_ai_tools_enabled(),
            summarizer_model: String::new(),
        }
    }
}

fn default_ai_model() -> String {
    "gpt-4o-mini".to_owned()
}

fn default_ai_tools_enabled() -> bool {
    true
}

fn default_preview_font_size() -> f32 {
    16.0
}

fn default_math_cap_scale() -> f32 {
    1.15
}

fn default_snap_to_grid() -> bool {
    true
}

fn default_show_grid() -> bool {
    true
}

/// Loads and saves [`Settings`] atomically. Missing or corrupt files fall back
/// to [`Settings::default`].
#[derive(Clone, Debug)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let path = root.into().join(".floatdea/settings.json");
        Ok(Self { path })
    }

    pub fn load(&self) -> Settings {
        read_json(&self.path).unwrap_or_default()
    }

    pub fn save(&self, settings: &Settings) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        write_json_atomic(&self.path, settings)
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
                "floatdea-settings-store-{}-{nonce}",
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
    fn loads_defaults_when_file_is_missing() {
        let folder = TestFolder::new();
        let store = SettingsStore::open(&folder.0).unwrap();
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn persists_changes_across_reloads() {
        let folder = TestFolder::new();
        let store = SettingsStore::open(&folder.0).unwrap();
        let settings = Settings {
            theme: ThemeSetting::Dark,
            preview_font_size: 18.0,
            math_cap_scale: 1.4,
            snap_to_grid: false,
            show_grid: false,
            window_mode: WindowMode::Floating,
            ..Settings::default()
        };
        store.save(&settings).unwrap();

        let reloaded = SettingsStore::open(&folder.0).unwrap().load();
        assert_eq!(reloaded, settings);
        assert!(folder.0.join(".floatdea/settings.json").is_file());
    }

    #[test]
    fn falls_back_to_defaults_on_corrupt_file() {
        let folder = TestFolder::new();
        let store = SettingsStore::open(&folder.0).unwrap();
        fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        fs::write(&store.path, "not json").unwrap();
        assert_eq!(store.load(), Settings::default());
    }

    #[test]
    fn older_settings_without_new_fields_still_load() {
        let folder = TestFolder::new();
        let store = SettingsStore::open(&folder.0).unwrap();
        fs::create_dir_all(store.path.parent().unwrap()).unwrap();
        fs::write(&store.path, r#"{"version":1,"theme":"light"}"#).unwrap();
        let loaded = store.load();
        assert_eq!(loaded.theme, ThemeSetting::Light);
        assert_eq!(loaded.preview_font_size, default_preview_font_size());
        assert_eq!(loaded.math_cap_scale, default_math_cap_scale());
        assert!(loaded.snap_to_grid, "new fields default when missing");
        assert!(loaded.show_grid, "new fields default when missing");
        assert_eq!(
            loaded.window_mode,
            WindowMode::Native,
            "new fields default when missing"
        );
        assert!(!loaded.ai_enabled, "AI is off by default");
        assert_eq!(loaded.ai_provider, ProviderKind::Fake);
        assert_eq!(loaded.ai_model, default_ai_model());
        assert!(loaded.ai_base_url.is_empty());
        assert!(loaded.ai_api_key.is_empty());
    }

    #[test]
    fn ai_is_disabled_by_default() {
        let settings = Settings::default();
        assert!(!settings.ai_enabled);
        assert_eq!(settings.ai_provider, ProviderKind::Fake);
        // The default fake provider requires no endpoint or key.
        let config = settings.ai_provider_config();
        assert_eq!(config.kind, ProviderKind::Fake);
        assert!(config.base_url.is_none());
    }

    #[test]
    fn ai_provider_config_uses_the_stored_key() {
        let settings = Settings {
            ai_enabled: true,
            ai_provider: ProviderKind::OpenAiCompatible,
            ai_model: "gpt-test".to_owned(),
            ai_base_url: "https://example.test/v1".to_owned(),
            ai_api_key: "sk-secret".to_owned(),
            ..Settings::default()
        };
        let config = settings.ai_provider_config();
        assert_eq!(config.kind, ProviderKind::OpenAiCompatible);
        assert_eq!(config.model, "gpt-test");
        assert_eq!(
            config.base_url.as_deref(),
            Some("https://example.test/v1")
        );
        assert_eq!(config.api_key.as_deref(), Some("sk-secret"));
    }

    #[test]
    fn ai_provider_config_blank_key_stays_none() {
        let settings = Settings {
            ai_api_key: "   ".to_owned(),
            ..Settings::default()
        };
        assert_eq!(settings.ai_provider_config().api_key, None);
    }

    #[test]
    fn ai_settings_persist_across_reloads() {
        let folder = TestFolder::new();
        let store = SettingsStore::open(&folder.0).unwrap();
        let settings = Settings {
            ai_enabled: true,
            ai_provider: ProviderKind::Ollama,
            ai_model: "llama3.2".to_owned(),
            ai_base_url: "http://127.0.0.1:11434".to_owned(),
            ai_api_key: "ollama-no-key".to_owned(),
            ..Settings::default()
        };
        store.save(&settings).unwrap();

        let reloaded = SettingsStore::open(&folder.0).unwrap().load();
        assert_eq!(reloaded, settings);
    }
}
