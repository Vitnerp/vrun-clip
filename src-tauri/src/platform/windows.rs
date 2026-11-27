//! Windows implementation: clipboard listener, reading/writing formats,
//! and sending the paste keystroke to the previously focused window.
//!
//! Design notes (why this does not block the Snipping Tool like Ditto can):
//! * We listen with `AddClipboardFormatListener` (no legacy viewer chain).
//! * Updates are debounced (~120 ms) so apps that write the clipboard in
//!   several steps (Snipping Tool, Office) finish before we look at it.
//! * We open the clipboard only to *copy bytes* of the few formats we need,
//!   then close it immediately. Decoding/encoding happens on another thread.
//! * When text or files are present, images are not requested at all, so
//!   apps that render bitmaps lazily (Excel) are never asked to do so.

use super::WatchConfig;
use crate::model::{CapturedClip, ClipPayload, RawImage};
use anyhow::{bail, Context, Result};
use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::*;
use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::{DragQueryFileW, DROPFILES, HDROP};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const DEBOUNCE_TIMER: usize = 1;
const DEBOUNCE_MS: u32 = 120;

/// HWND of our hidden message window (also used as clipboard owner).
static MSG_HWND: AtomicIsize = AtomicIsize::new(0);

struct Formats {
    html: u32,
    rtf: u32,
    png: u32,
    exclude: u32,
    viewer_ignore: u32,
    can_include_history: u32,
    drop_effect: u32,
    internal: u32,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn formats() -> &'static Formats {
    static F: OnceLock<Formats> = OnceLock::new();
    F.get_or_init(|| unsafe {
        let reg = |name: &str| RegisterClipboardFormatW(wide(name).as_ptr());
        Formats {
            html: reg("HTML Format"),
            rtf: reg("Rich Text Format"),
            png: reg("PNG"),
            exclude: reg("ExcludeClipboardContentFromMonitorProcessing"),
            viewer_ignore: reg("Clipboard Viewer Ignore"),
            can_include_history: reg("CanIncludeInClipboardHistory"),
            drop_effect: reg("Preferred DropEffect"),
            // Marks clipboard content written by vrun.clip itself.
            internal: reg("VrunClip.Internal"),
        }
    })
}

type Callback = Box<dyn Fn(CapturedClip) + Send>;

struct ListenerState {
    on_clip: Callback,
    config: Arc<WatchConfig>,
    source_app: Option<String>,
}

thread_local! {
    static STATE: RefCell<Option<ListenerState>> = const { RefCell::new(None) };
}

/// Starts the clipboard listener on its own thread with its own message loop.
pub fn start_watcher(config: Arc<WatchConfig>, on_clip: impl Fn(CapturedClip) + Send + 'static) {
    std::thread::Builder::new()
        .name("clipboard-listener".into())
        .spawn(move || unsafe {
            let _ = formats();
            STATE.with(|s| {
                *s.borrow_mut() = Some(ListenerState { on_clip: Box::new(on_clip), config, source_app: None })
            });
            let class = wide("VrunClipListener");
            let hinstance = GetModuleHandleW(std::ptr::null());
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinstance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(
                0,
                class.as_ptr(),
                wide("vrun.clip listener").as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            );
            if hwnd.is_null() {
                log::error!("failed to create clipboard listener window");
                return;
            }
            MSG_HWND.store(hwnd as isize, Ordering::SeqCst);
            if AddClipboardFormatListener(hwnd) == 0 {
                log::error!("AddClipboardFormatListener failed");
            }
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })
        .expect("spawn clipboard listener");
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CLIPBOARDUPDATE => {
                // Remember who copied *now*, read the data a bit later.
                let app = clipboard_source_app();
                STATE.with(|s| {
                    if let Some(st) = s.borrow_mut().as_mut() {
                        st.source_app = app;
                    }
                });
                SetTimer(hwnd, DEBOUNCE_TIMER, DEBOUNCE_MS, None);
                0
            }
            WM_TIMER if wparam == DEBOUNCE_TIMER => {
                KillTimer(hwnd, DEBOUNCE_TIMER);
                STATE.with(|s| {
                    if let Some(st) = s.borrow().as_ref() {
                        if st.config.paused.load(Ordering::Relaxed) {
                            return;
                        }
                        let max = st.config.max_bytes.load(Ordering::Relaxed);
                        match read_clipboard(hwnd, max) {
                            Ok(Some(mut clip)) => {
                                clip.source_app = st.source_app.clone();
                                (st.on_clip)(clip);
                            }
                            Ok(None) => {}
                            Err(e) => log::warn!("clipboard read failed: {e:#}"),
                        }
                    }
                });
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Opens the clipboard, retrying briefly if another app holds it.
fn open_clipboard(hwnd: HWND) -> Result<ClipboardGuard> {
    for attempt in 0..12 {
        if unsafe { OpenClipboard(hwnd) } != 0 {
            return Ok(ClipboardGuard);
        }
        std::thread::sleep(Duration::from_millis(5 + attempt * 5));
    }
    bail!("clipboard is busy")
}

struct ClipboardGuard;
impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        unsafe { CloseClipboard() };
    }
}

fn available(format: u32) -> bool {
    unsafe { IsClipboardFormatAvailable(format) != 0 }
}

/// Copies the bytes of a clipboard format. Must be called while open.
fn get_bytes(format: u32, max: usize) -> Option<Vec<u8>> {
    unsafe {
        let h = GetClipboardData(format);
        if h.is_null() {
            return None;
        }
        let size = GlobalSize(h);
        if size == 0 || size > max {
            return None;
        }
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return None;
        }
        let v = std::slice::from_raw_parts(p, size).to_vec();
        GlobalUnlock(h);
        Some(v)
    }
}

fn trim_nul(mut v: Vec<u8>) -> Vec<u8> {
    while v.last() == Some(&0) {
        v.pop();
    }
    v
}

fn read_clipboard(hwnd: HWND, max: usize) -> Result<Option<CapturedClip>> {
    let f = formats();
    // Cheap checks first, without opening the clipboard.
    if available(f.internal) || available(f.exclude) || available(f.viewer_ignore) {
        return Ok(None);
    }
    let _guard = open_clipboard(hwnd)?;

    if available(f.can_include_history)
        && let Some(b) = get_bytes(f.can_include_history, 64)
        && b.len() >= 4
        && u32::from_le_bytes(b[..4].try_into().unwrap()) == 0
    {
        return Ok(None); // e.g. password managers
    }

    let mut clip = CapturedClip::default();
    if available(CF_HDROP as u32) {
        clip.files = read_files();
    }
    if available(CF_UNICODETEXT as u32) {
        clip.text = get_bytes(CF_UNICODETEXT as u32, max).map(|b| {
            let units: Vec<u16> = b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
            String::from_utf16_lossy(&units[..end])
        });
    }
    if clip.files.is_none() && clip.text.is_some() {
        if available(f.html) {
            clip.html = get_bytes(f.html, max).map(trim_nul);
        }
        if available(f.rtf) {
            clip.rtf = get_bytes(f.rtf, max).map(trim_nul);
        }
    }
    if clip.files.is_none() && clip.text.is_none() {
        clip.image = if available(f.png) {
            get_bytes(f.png, max).map(RawImage::Png)
        } else if available(CF_DIBV5 as u32) {
            get_bytes(CF_DIBV5 as u32, max).map(RawImage::Dib)
        } else if available(CF_DIB as u32) {
            get_bytes(CF_DIB as u32, max).map(RawImage::Dib)
        } else {
            None
        };
    }
    // _guard drops here: clipboard closed before any heavy processing.
    if clip.files.is_none() && clip.text.is_none() && clip.image.is_none() {
        return Ok(None);
    }
    Ok(Some(clip))
}

fn read_files() -> Option<Vec<String>> {
    unsafe {
        let h = GetClipboardData(CF_HDROP as u32) as HDROP;
        if h.is_null() {
            return None;
        }
        let count = DragQueryFileW(h, u32::MAX, std::ptr::null_mut(), 0);
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let len = DragQueryFileW(h, i, std::ptr::null_mut(), 0);
            let mut buf = vec![0u16; len as usize + 1];
            DragQueryFileW(h, i, buf.as_mut_ptr(), buf.len() as u32);
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        Some(out)
    }
}

fn set_bytes(format: u32, data: &[u8]) -> Result<()> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, data.len().max(1));
        if h.is_null() {
            bail!("GlobalAlloc failed");
        }
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            GlobalFree(h);
            bail!("GlobalLock failed");
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        GlobalUnlock(h);
        if SetClipboardData(format, h).is_null() {
            GlobalFree(h);
            bail!("SetClipboardData({format}) failed");
        }
        Ok(())
    }
}

fn utf16_bytes(s: &str) -> Vec<u8> {
    s.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect()
}

fn hdrop_bytes(files: &[String]) -> Vec<u8> {
    let header = size_of::<DROPFILES>();
    let mut out = vec![0u8; header];
    out[0..4].copy_from_slice(&(header as u32).to_le_bytes()); // pFiles
    out[16..20].copy_from_slice(&1u32.to_le_bytes()); // fWide
    for f in files {
        out.extend(f.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes));
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// Puts a stored item on the clipboard. With `plain`, only text is written.
pub fn write_clipboard(payload: &ClipPayload, plain: bool) -> Result<()> {
    // Prepare everything (PNG decode can take ~50 ms for 4K) *before*
    // opening the clipboard, so we hold it for as short as possible.
    let image = match &payload.image_png_path {
        Some(path) => {
            let png = std::fs::read(path).context("read stored image")?;
            let rgba = image::load_from_memory_with_format(&png, image::ImageFormat::Png)?.to_rgba8();
            Some((png, crate::capture::rgba_to_dib(&rgba)))
        }
        None => None,
    };
    let f = formats();
    let hwnd = MSG_HWND.load(Ordering::SeqCst) as HWND;
    let _guard = open_clipboard(hwnd)?;
    unsafe { EmptyClipboard() };
    if let Some(text) = &payload.text {
        set_bytes(CF_UNICODETEXT as u32, &utf16_bytes(text))?;
    }
    if !plain {
        if let Some(html) = &payload.html {
            let mut h = html.clone();
            h.push(0);
            set_bytes(f.html, &h)?;
        }
        if let Some(rtf) = &payload.rtf {
            let mut r = rtf.clone();
            r.push(0);
            set_bytes(f.rtf, &r)?;
        }
        if let Some(files) = &payload.files {
            set_bytes(CF_HDROP as u32, &hdrop_bytes(files))?;
            set_bytes(f.drop_effect, &1u32.to_le_bytes())?; // DROPEFFECT_COPY
        }
    }
    if let Some((png, dib)) = &image {
        set_bytes(CF_DIB as u32, dib)?;
        set_bytes(f.png, png)?;
    }
    set_bytes(f.internal, &[1])?;
    Ok(())
}

fn exe_name_of_window(hwnd: HWND) -> Option<String> {
    unsafe {
        if hwnd.is_null() {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return None;
        }
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len);
        CloseHandle(process);
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit('\\').next().map(str::to_string)
    }
}

/// Executable name of the app that just changed the clipboard.
fn clipboard_source_app() -> Option<String> {
    unsafe { exe_name_of_window(GetClipboardOwner()).or_else(|| exe_name_of_window(GetForegroundWindow())) }
}

pub fn foreground_window() -> isize {
    unsafe { GetForegroundWindow() as isize }
}

fn key(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Focuses `target` and sends Ctrl+V to it.
pub fn paste_into(target: isize) {
    unsafe {
        let hwnd = target as HWND;
        if hwnd.is_null() || IsWindow(hwnd) == 0 {
            return;
        }
        SetForegroundWindow(hwnd);
        for _ in 0..50 {
            if GetForegroundWindow() == hwnd {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(30));
        let mut inputs = Vec::new();
        // Shift may still be held from Shift+Enter (paste as plain text).
        for vk in [VK_SHIFT, VK_LSHIFT, VK_RSHIFT] {
            if GetAsyncKeyState(vk as i32) < 0 {
                inputs.push(key(vk, true));
            }
        }
        const VK_V: u16 = 0x56;
        inputs.extend([key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)]);
        SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32);
    }
}

/// Non-blocking native message box.
pub fn message_box(title: &str, text: &str) {
    let (title, text) = (wide(title), wide(text));
    std::thread::spawn(move || unsafe {
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONWARNING | MB_SETFOREGROUND);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    /// Writes to the real system clipboard, so it is opt-in:
    /// `cargo test -- --ignored clipboard_roundtrip`
    #[test]
    #[ignore]
    fn clipboard_roundtrip() {
        let cfg = Arc::new(WatchConfig { paused: AtomicBool::new(true), max_bytes: AtomicUsize::new(usize::MAX) });
        start_watcher(cfg, |_| {});
        std::thread::sleep(Duration::from_millis(300));
        let hwnd = MSG_HWND.load(Ordering::SeqCst) as HWND;
        assert!(!hwnd.is_null());

        // Image: stored PNG -> CF_DIB + PNG on the clipboard.
        let img = image::RgbaImage::from_fn(640, 360, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]));
        let png_path = std::env::temp_dir().join("vrun-clip-test.png");
        std::fs::write(&png_path, crate::capture::encode_png(&img).unwrap()).unwrap();
        write_clipboard(&ClipPayload { image_png_path: Some(png_path), ..Default::default() }, false).unwrap();
        {
            let _g = open_clipboard(hwnd).unwrap();
            assert!(available(formats().internal), "own writes are marked");
            let dib = get_bytes(CF_DIB as u32, usize::MAX).unwrap();
            let back = crate::capture::dib_to_rgba(&dib).unwrap();
            assert_eq!(back.dimensions(), (640, 360));
            assert_eq!(back.get_pixel(300, 200), img.get_pixel(300, 200));
        }

        // Files + text + html.
        let files = vec![r"C:\Windows\notepad.exe".to_string(), r"C:\Windows\win.ini".to_string()];
        write_clipboard(
            &ClipPayload {
                text: Some("Příliš žluťoučký kůň".into()),
                html: Some(b"Version:0.9\r\n<b>x</b>".to_vec()),
                files: Some(files.clone()),
                ..Default::default()
            },
            false,
        )
        .unwrap();
        {
            let _g = open_clipboard(hwnd).unwrap();
            assert_eq!(read_files().unwrap(), files);
            assert!(available(formats().html));
        }

        // Plain mode drops rich formats.
        write_clipboard(&ClipPayload { text: Some("plain".into()), html: Some(b"<b>x</b>".to_vec()), ..Default::default() }, true)
            .unwrap();
        let _g = open_clipboard(hwnd).unwrap();
        assert!(available(CF_UNICODETEXT as u32));
        assert!(!available(formats().html));
    }
}
