mod capture;
mod db;
mod model;
mod platform;
mod settings;
mod updater;

use db::Store;
use model::*;
use platform::WatchConfig;
use serde::Serialize;
use settings::Settings;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use tauri::http::Response;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, RunEvent, State, WindowEvent, Wry};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

const POPUP: &str = "main";
const POPUP_W: f64 = 760.0;
const POPUP_H: f64 = 480.0;

pub struct AppState {
    store: Arc<Mutex<Store>>,
    settings: Mutex<Settings>,
    data_dir: PathBuf,
    watch: Arc<WatchConfig>,
    /// Window that was focused when the hotkey was pressed (paste target).
    prev_window: AtomicIsize,
    hotkey_error: Mutex<Option<String>>,
    tray: Mutex<Option<TrayChecks>>,
}

struct TrayChecks {
    pause: CheckMenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
    update: MenuItem<Wry>,
}

/// Keeps the tray "update" menu entry in sync with the updater.
pub(crate) fn on_update_status(app: &AppHandle, status: &updater::UpdateStatus) {
    use updater::Phase;
    let text = match status.phase {
        Phase::Ready => format!("Restart to update to {}", status.version.as_deref().unwrap_or("new version")),
        Phase::Checking => "Checking for updates…".into(),
        Phase::Downloading => "Downloading update…".into(),
        _ => "Check for updates".into(),
    };
    if let Some(t) = app.state::<AppState>().tray.lock().unwrap().as_ref() {
        let _ = t.update.set_text(text);
    }
}

fn update_action(app: &AppHandle) {
    if app.state::<updater::UpdateState>().status().phase == updater::Phase::Ready {
        if let Err(e) = updater::install(app) {
            platform::message_box("vrun.clip update", &e);
        }
    } else {
        tauri::async_runtime::spawn(updater::check(app.clone()));
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Placement {
    Cursor,
    Center,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PopupShown {
    welcome: bool,
    settings: bool,
    can_paste: bool,
}

fn show_popup(app: &AppHandle, placement: Placement, from_hotkey: bool, payload: PopupShown) {
    let Some(win) = app.get_webview_window(POPUP) else { return };
    let state = app.state::<AppState>();

    if from_hotkey && win.is_visible().unwrap_or(false) && win.is_focused().unwrap_or(false) {
        let _ = win.hide();
        return;
    }

    let target = if from_hotkey { platform::foreground_window() } else { 0 };
    #[cfg(windows)]
    let own = win.hwnd().map(|h| h.0 as isize).unwrap_or(0);
    #[cfg(not(windows))]
    let own = 0;
    if target != own {
        state.prev_window.store(target, Ordering::SeqCst);
    }

    if let Some(pos) = popup_position(app, placement) {
        let _ = win.set_position(pos);
    }
    let _ = win.show();
    let _ = win.unminimize();
    let _ = win.set_focus();
    let payload = PopupShown { can_paste: state.prev_window.load(Ordering::SeqCst) != 0, ..payload };
    let _ = win.emit_to(POPUP, "popup-shown", payload);
}

/// Top-left corner at the mouse cursor, kept inside the monitor work area.
fn popup_position(app: &AppHandle, placement: Placement) -> Option<PhysicalPosition<i32>> {
    let cursor = app.cursor_position().ok()?;
    let monitor = app
        .monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .or_else(|| app.primary_monitor().ok().flatten())?;
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let (w, h) = ((POPUP_W * scale) as i32, (POPUP_H * scale) as i32);
    let (ax, ay) = (area.position.x, area.position.y);
    let (aw, ah) = (area.size.width as i32, area.size.height as i32);
    let (x, y) = match placement {
        Placement::Cursor => (cursor.x as i32 + 4, cursor.y as i32 + 4),
        Placement::Center => (ax + (aw - w) / 2, ay + (ah - h) / 3),
    };
    let x = x.min(ax + aw - w).max(ax);
    let y = y.min(ay + ah - h).max(ay);
    Some(PhysicalPosition::new(x, y))
}

fn shown(welcome: bool, settings: bool) -> PopupShown {
    PopupShown { welcome, settings, can_paste: false }
}

fn apply_hotkey(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    let shortcut: Shortcut = hotkey
        .parse()
        .map_err(|e| format!("\"{hotkey}\" is not a valid shortcut ({e})."))?;
    gs.register(shortcut).map_err(|e| {
        format!("Could not register {hotkey}. Another app (for example Ditto) probably uses it. ({e})")
    })?;
    if let Some(tray) = app.tray_by_id("main") {
        let _ = tray.set_tooltip(Some(format!("vrun.clip — {hotkey}")));
    }
    Ok(())
}

fn apply_autostart(app: &AppHandle, enabled: bool) {
    if cfg!(debug_assertions) {
        log::info!("dev build: not changing autostart (wanted {enabled})");
        return;
    }
    let al = app.autolaunch();
    let current = al.is_enabled().unwrap_or(false);
    let res = match (enabled, current) {
        (true, false) => al.enable(),
        (false, true) => al.disable(),
        _ => Ok(()),
    };
    if let Err(e) = res {
        log::warn!("autostart change failed: {e}");
    }
}

/// Applies (and persists) new settings. Returns the hotkey error, if any.
fn apply_settings(app: &AppHandle, new: Settings) -> Option<String> {
    let state = app.state::<AppState>();
    let old = state.settings.lock().unwrap().clone();
    let hotkey_error = if old.hotkey != new.hotkey || state.hotkey_error.lock().unwrap().is_some() {
        apply_hotkey(app, &new.hotkey).err()
    } else {
        None
    };
    *state.hotkey_error.lock().unwrap() = hotkey_error.clone();
    if old.autostart != new.autostart {
        apply_autostart(app, new.autostart);
    }
    state.watch.paused.store(new.paused, Ordering::Relaxed);
    state.watch.max_bytes.store(new.max_item_mb as usize * 1024 * 1024, Ordering::Relaxed);
    if let Some(t) = state.tray.lock().unwrap().as_ref() {
        let _ = t.pause.set_checked(new.paused);
        let _ = t.autostart.set_checked(new.autostart);
    }
    if let Err(e) = new.save(&state.data_dir) {
        log::error!("saving settings failed: {e:#}");
    }
    let _ = state.store.lock().unwrap().prune(new.max_items, new.max_image_mb as i64 * 1024 * 1024);
    *state.settings.lock().unwrap() = new;
    hotkey_error
}

// ---------------------------------------------------------------- commands

type CmdResult<T> = Result<T, String>;

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
async fn search(state: State<'_, AppState>, query: String, offset: u32, limit: u32) -> CmdResult<Vec<ClipSummary>> {
    state.store.lock().unwrap().search(&query, offset, limit).map_err(err)
}

#[tauri::command]
async fn detail(state: State<'_, AppState>, id: i64) -> CmdResult<ClipDetail> {
    state.store.lock().unwrap().detail(id).map_err(err)
}

/// Puts the item on the clipboard, hides the popup and (if `paste` and we
/// know the previous window) sends Ctrl+V to it.
#[tauri::command]
async fn activate(app: AppHandle, state: State<'_, AppState>, id: i64, plain: bool, paste: bool) -> CmdResult<()> {
    let payload = state.store.lock().unwrap().payload(id).map_err(err)?;
    platform::write_clipboard(&payload, plain).map_err(err)?;
    let _ = state.store.lock().unwrap().touch(id);
    if let Some(win) = app.get_webview_window(POPUP) {
        let _ = win.hide();
    }
    let target = state.prev_window.swap(0, Ordering::SeqCst);
    if paste && target != 0 {
        std::thread::spawn(move || platform::paste_into(target));
    }
    Ok(())
}

#[tauri::command]
async fn delete_item(state: State<'_, AppState>, id: i64) -> CmdResult<()> {
    state.store.lock().unwrap().delete(id).map_err(err)
}

#[tauri::command]
async fn toggle_pin(state: State<'_, AppState>, id: i64) -> CmdResult<bool> {
    state.store.lock().unwrap().toggle_pin(id).map_err(err)
}

#[tauri::command]
async fn clear_history(state: State<'_, AppState>, keep_pinned: bool) -> CmdResult<usize> {
    state.store.lock().unwrap().clear(keep_pinned).map_err(err)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    settings: Settings,
    hotkey_error: Option<String>,
    stats: Stats,
    version: String,
}

fn settings_view(app: &AppHandle) -> CmdResult<SettingsView> {
    let state = app.state::<AppState>();
    let stats = state.store.lock().unwrap().stats().map_err(err)?;
    Ok(SettingsView {
        settings: state.settings.lock().unwrap().clone(),
        hotkey_error: state.hotkey_error.lock().unwrap().clone(),
        stats,
        version: app.package_info().version.to_string(),
    })
}

#[tauri::command]
async fn get_settings(app: AppHandle) -> CmdResult<SettingsView> {
    settings_view(&app)
}

#[tauri::command]
async fn save_settings(app: AppHandle, settings: Settings) -> CmdResult<SettingsView> {
    let current = app.state::<AppState>().settings.lock().unwrap().clone();
    let new = Settings { welcome_done: current.welcome_done, version: current.version, ..settings }.sanitized();
    apply_settings(&app, new);
    settings_view(&app)
}

/// Temporarily releases the global shortcut while the user records a new one.
#[tauri::command]
async fn suspend_hotkey(app: AppHandle, suspend: bool) -> CmdResult<()> {
    if suspend {
        let _ = app.global_shortcut().unregister_all();
    } else {
        let hotkey = app.state::<AppState>().settings.lock().unwrap().hotkey.clone();
        let e = apply_hotkey(&app, &hotkey).err();
        *app.state::<AppState>().hotkey_error.lock().unwrap() = e;
    }
    Ok(())
}

#[tauri::command]
fn update_status(state: State<'_, updater::UpdateState>) -> updater::UpdateStatus {
    state.status()
}

#[tauri::command]
fn check_update(app: AppHandle) {
    tauri::async_runtime::spawn(updater::check(app));
}

#[tauri::command]
fn install_update(app: AppHandle) -> CmdResult<()> {
    updater::install(&app)
}

#[tauri::command]
fn hide_popup(app: AppHandle) {
    if let Some(win) = app.get_webview_window(POPUP) {
        let _ = win.hide();
    }
}

#[tauri::command]
async fn open_data_dir(app: AppHandle) -> CmdResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let dir = app.state::<AppState>().data_dir.display().to_string();
    app.opener().open_path(dir, None::<&str>).map_err(err)
}

// ------------------------------------------------------------------- setup

fn start_capture(app: &AppHandle) {
    let state = app.state::<AppState>();
    let (tx, rx) = mpsc::channel::<CapturedClip>();
    platform::start_watcher(state.watch.clone(), move |clip| {
        let _ = tx.send(clip);
    });
    let app = app.clone();
    std::thread::Builder::new()
        .name("capture-worker".into())
        .spawn(move || {
            let state = app.state::<AppState>();
            for clip in rx {
                let settings = state.settings.lock().unwrap().clone();
                if settings.paused || settings.is_ignored(clip.source_app.as_deref()) {
                    continue;
                }
                match capture::process(&state.store, &settings, clip) {
                    Ok(Some((_id, is_new))) => {
                        if is_new {
                            let _ = state
                                .store
                                .lock()
                                .unwrap()
                                .prune(settings.max_items, settings.max_image_mb as i64 * 1024 * 1024);
                        }
                        let _ = app.emit_to(POPUP, "clips-changed", ());
                    }
                    Ok(None) => {}
                    Err(e) => log::warn!("capture failed: {e:#}"),
                }
            }
        })
        .expect("spawn capture worker");
}

fn build_tray(app: &AppHandle, settings: &Settings) -> tauri::Result<TrayChecks> {
    let open = MenuItem::with_id(app, "open", "Open vrun.clip", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause recording", true, settings.paused, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(app, "autostart", "Start with Windows", true, settings.autostart, None::<&str>)?;
    let prefs = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let update = MenuItem::with_id(app, "update", "Check for updates", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit vrun.clip", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &PredefinedMenuItem::separator(app)?,
            &pause,
            &autostart,
            &prefs,
            &update,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip(format!("vrun.clip — {}", settings.hotkey))
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let state = app.state::<AppState>();
            match event.id().as_ref() {
                "open" => show_popup(app, Placement::Cursor, false, shown(false, false)),
                "settings" => show_popup(app, Placement::Center, false, shown(false, true)),
                "pause" => {
                    let mut s = state.settings.lock().unwrap().clone();
                    s.paused = !s.paused;
                    apply_settings(app, s);
                }
                "autostart" => {
                    let mut s = state.settings.lock().unwrap().clone();
                    s.autostart = !s.autostart;
                    apply_settings(app, s);
                }
                "update" => update_action(app),
                "quit" => app.exit(0),
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_popup(tray.app_handle(), Placement::Cursor, false, shown(false, false));
            }
        })
        .build(app)?;
    Ok(TrayChecks { pause, autostart, update })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        // Must be first: a second launch just opens the running instance.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_popup(app, Placement::Center, false, shown(false, false));
        }))
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .max_file_size(2_000_000)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updater::UpdateState::default())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        show_popup(app, Placement::Cursor, true, shown(false, false));
                    }
                })
                .build(),
        )
        .register_asynchronous_uri_scheme_protocol("vclip", |ctx, request, responder| {
            let app = ctx.app_handle().clone();
            std::thread::spawn(move || {
                let file = app.state::<AppState>().store.lock().unwrap().asset_file(request.uri().path());
                let response = match file.and_then(|f| std::fs::read(f).ok()) {
                    Some(bytes) => Response::builder()
                        .header("Content-Type", "image/png")
                        .header("Cache-Control", "max-age=31536000, immutable")
                        .body(bytes),
                    None => Response::builder().status(404).body(Vec::new()),
                };
                responder.respond(response.expect("valid response"));
            });
        })
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let (settings, existed) = Settings::load(&data_dir);
            let settings = settings.sanitized();
            let store = Store::open(&data_dir)?;
            app.manage(AppState {
                store: Arc::new(Mutex::new(store)),
                settings: Mutex::new(settings.clone()),
                data_dir: data_dir.clone(),
                watch: Arc::new(WatchConfig {
                    paused: AtomicBool::new(settings.paused),
                    max_bytes: AtomicUsize::new(settings.max_item_mb as usize * 1024 * 1024),
                }),
                prev_window: AtomicIsize::new(0),
                hotkey_error: Mutex::new(None),
                tray: Mutex::new(None),
            });
            if !existed {
                let _ = settings.save(&data_dir);
            }

            let checks = build_tray(&handle, &settings)?;
            *app.state::<AppState>().tray.lock().unwrap() = Some(checks);

            if let Err(e) = apply_hotkey(&handle, &settings.hotkey) {
                log::warn!("{e}");
                *app.state::<AppState>().hotkey_error.lock().unwrap() = Some(e.clone());
                platform::message_box("vrun.clip", &format!("{e}\n\nYou can pick another shortcut in Settings (tray icon → Settings…)."));
            }
            apply_autostart(&handle, settings.autostart);
            start_capture(&handle);
            updater::start_schedule(&handle);

            let autostarted = std::env::args().any(|a| a == "--autostart");
            if !settings.welcome_done && !autostarted {
                let mut s = settings.clone();
                s.welcome_done = true;
                apply_settings(&handle, s);
                // Give the webview a moment to load before showing.
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(600));
                    let h2 = h.clone();
                    let _ = h.run_on_main_thread(move || {
                        show_popup(&h2, Placement::Center, false, shown(true, false));
                    });
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::Focused(false) if window.label() == POPUP => {
                let _ = window.hide();
            }
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            search,
            detail,
            activate,
            delete_item,
            toggle_pin,
            clear_history,
            get_settings,
            save_settings,
            suspend_hotkey,
            hide_popup,
            open_data_dir,
            update_status,
            check_update,
            install_update
        ])
        .build(tauri::generate_context!())
        .expect("error while building vrun.clip");

    app.run(|_app, event| {
        // Closing the popup must not quit: we live in the tray.
        if let RunEvent::ExitRequested { api, code: None, .. } = event {
            api.prevent_exit();
        }
    });
}
