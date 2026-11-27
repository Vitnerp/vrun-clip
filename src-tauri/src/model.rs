//! Platform independent data types shared by capture, storage and UI.

use serde::{Deserialize, Serialize};

/// What kind of clip an entry is. Stored as text in the DB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Image,
    Files,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Image => "image",
            Kind::Files => "files",
        }
    }
    pub fn parse(s: &str) -> Kind {
        match s {
            "image" => Kind::Image,
            "files" => Kind::Files,
            _ => Kind::Text,
        }
    }
}

/// Raw image as it came from the OS clipboard. Conversion happens later,
/// on a worker thread, so the clipboard is released as fast as possible.
#[derive(Debug)]
pub enum RawImage {
    /// Already PNG encoded (e.g. the registered "PNG" clipboard format).
    Png(Vec<u8>),
    /// Windows device independent bitmap (BITMAPINFOHEADER/V5 + pixels).
    #[cfg_attr(not(windows), allow(dead_code))]
    Dib(Vec<u8>),
    /// TIFF, the classic macOS pasteboard image format.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Tiff(Vec<u8>),
}

/// Everything read from the clipboard in one go.
#[derive(Debug, Default)]
pub struct CapturedClip {
    pub text: Option<String>,
    /// Windows "HTML Format" payload (UTF-8 with the CF_HTML header).
    pub html: Option<Vec<u8>>,
    pub rtf: Option<Vec<u8>>,
    pub files: Option<Vec<String>>,
    pub image: Option<RawImage>,
    pub source_app: Option<String>,
}

/// Decoded content used when writing an entry back to the clipboard.
#[derive(Debug, Default)]
pub struct ClipPayload {
    pub text: Option<String>,
    pub html: Option<Vec<u8>>,
    pub rtf: Option<Vec<u8>>,
    pub files: Option<Vec<String>>,
    /// Absolute path to the stored PNG.
    pub image_png_path: Option<std::path::PathBuf>,
}

/// Row returned to the UI list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipSummary {
    pub id: i64,
    pub kind: Kind,
    pub preview: String,
    pub source_app: Option<String>,
    pub created_at: i64,
    pub used_at: i64,
    pub pinned: bool,
    pub size: i64,
    pub has_html: bool,
    pub has_rtf: bool,
    pub image_width: Option<i64>,
    pub image_height: Option<i64>,
    pub thumb_url: Option<String>,
    pub image_url: Option<String>,
}

/// Detail shown in the preview pane.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipDetail {
    pub id: i64,
    pub kind: Kind,
    /// Text, possibly truncated for display.
    pub text: String,
    pub truncated: bool,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub items: i64,
    pub pinned: i64,
    pub image_bytes: i64,
    pub data_dir: String,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
