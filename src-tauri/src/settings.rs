//! User settings, persisted as JSON next to the database.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Global shortcut that opens the popup, e.g. "Ctrl+Alt+V".
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
            hotkey: "Ctrl+Alt+V".into(),
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
            Ok(s) => (serde_json::from_str(&s).unwrap_or_default(), true),
            Err(_) => (Settings::default(), false),
        }
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
