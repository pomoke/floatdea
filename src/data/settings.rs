//! User settings persisted to `.floatdea/settings.json`. Settings are local to
//! the workspace and survive restarts; the UI lives in the system "设置"
//! (Settings) special item.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            theme: ThemeSetting::default(),
            preview_font_size: default_preview_font_size(),
            math_cap_scale: default_math_cap_scale(),
        }
    }
}

fn default_preview_font_size() -> f32 {
    16.0
}

fn default_math_cap_scale() -> f32 {
    1.15
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
    }
}
