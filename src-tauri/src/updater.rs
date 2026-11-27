//! Auto-update via GitHub Releases (`latest.json` produced by tauri-action).
//!
//! Updates are signed with the project's minisign key; the public key lives
//! in `tauri.conf.json` (`plugins.updater.pubkey`), so a tampered download is
//! rejected. Flow: check quietly → download in background → user clicks
//! "Restart to update" → installer runs in passive mode and relaunches us.

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

const FIRST_CHECK_DELAY: Duration = Duration::from_secs(20);
const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Downloading,
    Ready,
    Error,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub phase: Phase,
    pub version: Option<String>,
    pub notes: Option<String>,
    pub message: Option<String>,
}

#[derive(Default)]
pub struct UpdateState {
    status: Mutex<UpdateStatus>,
    pending: Mutex<Option<(Update, Vec<u8>)>>,
    busy: AtomicBool,
}

impl UpdateState {
    pub fn status(&self) -> UpdateStatus {
        self.status.lock().unwrap().clone()
    }
}

fn set_status(app: &AppHandle, status: UpdateStatus) {
    *app.state::<UpdateState>().status.lock().unwrap() = status.clone();
    crate::on_update_status(app, &status);
    let _ = app.emit("update-status", status);
}

/// Checks for an update and downloads it. Safe to call repeatedly.
pub async fn check(app: AppHandle) {
    let state = app.state::<UpdateState>();
    if state.status().phase == Phase::Ready || state.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    set_status(&app, UpdateStatus { phase: Phase::Checking, ..Default::default() });
    let result: Result<UpdateStatus, tauri_plugin_updater::Error> = async {
        let Some(update) = app.updater()?.check().await? else {
            return Ok(UpdateStatus { phase: Phase::UpToDate, ..Default::default() });
        };
        let version = update.version.clone();
        let notes = update.body.clone();
        set_status(
            &app,
            UpdateStatus { phase: Phase::Downloading, version: Some(version.clone()), ..Default::default() },
        );
        let bytes = update.download(|_, _| {}, || {}).await?;
        *app.state::<UpdateState>().pending.lock().unwrap() = Some((update, bytes));
        log::info!("update {version} downloaded and ready");
        Ok(UpdateStatus { phase: Phase::Ready, version: Some(version), notes, message: None })
    }
    .await;
    let status = result.unwrap_or_else(|e| {
        log::warn!("update check failed: {e}");
        UpdateStatus { phase: Phase::Error, message: Some(e.to_string()), ..Default::default() }
    });
    set_status(&app, status);
    app.state::<UpdateState>().busy.store(false, Ordering::SeqCst);
}

/// Runs the downloaded installer. On Windows this exits the app; the
/// installer relaunches the new version when done.
pub fn install(app: &AppHandle) -> Result<(), String> {
    let pending = app.state::<UpdateState>().pending.lock().unwrap().take();
    let Some((update, bytes)) = pending else {
        return Err("No update has been downloaded yet.".into());
    };
    update.install(bytes).map_err(|e| e.to_string())?;
    app.restart();
}

/// Background schedule: shortly after start, then every few hours.
/// Disabled in dev builds.
pub fn start_schedule(app: &AppHandle) {
    if cfg!(debug_assertions) {
        return;
    }
    let app = app.clone();
    std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            std::thread::sleep(FIRST_CHECK_DELAY);
            loop {
                tauri::async_runtime::block_on(check(app.clone()));
                std::thread::sleep(CHECK_INTERVAL);
            }
        })
        .expect("spawn update thread");
}
