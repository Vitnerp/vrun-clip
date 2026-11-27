//! User settings, persisted as JSON next to the database.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bump when a migration in `Settings::migrate` is added.
const SETTINGS_VERSION: u32 = 2;
const OLD_DEFAULT_HOTKEY: &str = "Ctrl+Alt+V";

/// Ctrl+Shift+V on Windows/Linux, Cmd+Shift+V on macOS.
pub fn default_hotkey() -> &'static str {
    if cfg!(target_os = "macos") { "Cmd+Shift+V" } else { "Ctrl+Shift+V" }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Schema version of this file. Missing in files written by 0.1.0/0.1.1.
    #[serde(default)]
    pub version: u32,
    /// Global shortcut that opens the popup, e.g. "Ctrl+Shift+V".
    pub hotkey: String,
    /// Maximum number of unpinned items kept in history.
    pub max_items: u32,
    /// Maximum disk space used by unpinned images, in megabytes.
    pub max_image_mb: u32,
    /// Clips larger than this (in megabytes) are ignored.
    pub max_item_mb: u32,
    /// Temporarily stop recording the clipboard.
    pub paused: bool,
    /// Launch vrun.clip when Windows starts.
    pub autostart: bool,
    /// Executable names (case-insensitive) whose clipboard is never recorded.
    pub ignored_apps: Vec<String>,
    /// Set after the welcome screen has been shown once.
    pub welcome_done: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            hotkey: default_hotkey().into(),
            max_items: 5000,
            max_image_mb: 2048,
            max_item_mb: 64,
            paused: false,
            autostart: true,
            ignored_apps: vec![
                "KeePass.exe".into(),
                "KeePassXC.exe".into(),
                "1Password.exe".into(),
                "Bitwarden.exe".into(),
                // macOS executable names
                "KeePassXC".into(),
                "1Password".into(),
                "Bitwarden".into(),
            ],
            welcome_done: false,
        }
    }
}

impl Settings {
    pub fn file(dir: &Path) -> PathBuf {
        dir.join("settings.json")
    }

    /// Returns the settings and whether the file existed (false = first run).
    pub fn load(dir: &Path) -> (Settings, bool) {
        let path = Self::file(dir);
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                let mut settings: Settings = serde_json::from_str(&s).unwrap_or_default();
                if settings.migrate() {
                    let _ = settings.save(dir);
                }
                (settings, true)
            }
            Err(_) => (Settings::default(), false),
        }
    }

    /// Upgrades settings written by older versions. Returns true if changed.
    fn migrate(&mut self) -> bool {
        if self.version >= SETTINGS_VERSION {
            return false;
        }
        // v2: default shortcut changed from Ctrl+Alt+V to Ctrl+Shift+V.
        // Only users still on the old default are moved.
        if self.version < 2 && self.hotkey.eq_ignore_ascii_case(OLD_DEFAULT_HOTKEY) {
            self.hotkey = default_hotkey().into();
        }
        self.version = SETTINGS_VERSION;
        true
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        let path = Self::file(dir);
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    }

    /// Clamp values coming from the UI to sane ranges.
    pub fn sanitized(mut self) -> Self {
        self.max_items = self.max_items.clamp(10, 1_000_000);
        self.max_image_mb = self.max_image_mb.clamp(10, 1_000_000);
        self.max_item_mb = self.max_item_mb.clamp(1, 1024);
        self.hotkey = self.hotkey.trim().to_string();
        self.ignored_apps = self
            .ignored_apps
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        self
    }

    pub fn is_ignored(&self, app: Option<&str>) -> bool {
        match app {
            Some(app) => self.ignored_apps.iter().any(|a| a.eq_ignore_ascii_case(app)),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("vrun-clip-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn old_default_hotkey_is_migrated_and_saved() {
        let d = dir();
        std::fs::write(Settings::file(&d), r#"{"hotkey":"Ctrl+Alt+V","maxItems":123}"#).unwrap();
        let (s, existed) = Settings::load(&d);
        assert!(existed);
        assert_eq!(s.hotkey, default_hotkey());
        assert_eq!(s.max_items, 123, "other values kept");
        assert_eq!(s.version, SETTINGS_VERSION);
        let saved = std::fs::read_to_string(Settings::file(&d)).unwrap();
        assert!(saved.contains(default_hotkey()));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn custom_hotkey_is_kept() {
        let d = dir();
        std::fs::write(Settings::file(&d), r#"{"hotkey":"Ctrl+`"}"#).unwrap();
        assert_eq!(Settings::load(&d).0.hotkey, "Ctrl+`");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn new_install_defaults() {
        let d = dir();
        let (s, existed) = Settings::load(&d);
        assert!(!existed);
        assert_eq!(s.hotkey, default_hotkey());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn hotkeys_parse() {
        for k in ["Ctrl+Shift+V", "Ctrl+Alt+V", "Ctrl+Backquote", "Cmd+Shift+V", "Super+Shift+V"] {
            assert!(k.parse::<tauri_plugin_global_shortcut::Shortcut>().is_ok(), "{k}");
        }
    }
}
