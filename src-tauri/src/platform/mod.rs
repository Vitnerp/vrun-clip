//! OS specific clipboard access. Each platform exposes the same functions:
//! `start_watcher`, `write_clipboard`, `foreground_window`, `paste_into`,
//! `message_box`. Implemented: Windows (Win32) and macOS (NSPasteboard
//! polling + CGEvent). Linux (wl-clipboard / X11) slots in here later.

use std::sync::atomic::{AtomicBool, AtomicUsize};

/// Live settings the listener thread reads without locking.
pub struct WatchConfig {
    pub paused: AtomicBool,
    /// Maximum bytes copied from a single clipboard format.
    pub max_bytes: AtomicUsize,
}

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(not(any(windows, target_os = "macos")))]
mod unsupported {
    use super::WatchConfig;
    use crate::model::{CapturedClip, ClipPayload};
    use std::sync::Arc;

    pub fn start_watcher(_config: Arc<WatchConfig>, _on_clip: impl Fn(CapturedClip) + Send + 'static) {
        log::warn!("clipboard watching is not implemented on this platform yet");
    }
    pub fn write_clipboard(_payload: &ClipPayload, _plain: bool) -> anyhow::Result<()> {
        anyhow::bail!("not implemented on this platform yet")
    }
    pub fn foreground_window() -> isize {
        0
    }
    pub fn paste_into(_target: isize) {}
    pub fn message_box(title: &str, text: &str) {
        log::warn!("{title}: {text}");
    }
}
#[cfg(not(any(windows, target_os = "macos")))]
pub use unsupported::*;
