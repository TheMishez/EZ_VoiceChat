//! In-game overlay: a small transparent, click-through window in a corner of the screen that
//! shows who's talking. It only exists while you're connected and (by default) while a
//! fullscreen game is in front, so it costs nothing the rest of the time.
//!
//! Games must run in borderless / "fullscreen windowed" mode for any overlay to show on top.
use crate::util;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// "tl" | "tr" | "bl" | "br"
    pub corner: String,
    /// "talking": only people speaking right now; "channel": everyone in your channel
    pub show: String,
    /// Only show over a fullscreen app (your game), not on the desktop.
    pub only_fullscreen: bool,
    pub scale: f32,
    pub opacity: f32,
    /// Keep it out of OBS / Discord screen share.
    pub hide_from_capture: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { enabled: true, corner: "tl".into(), show: "talking".into(), only_fullscreen: true, scale: 1.0, opacity: 0.9, hide_from_capture: false }
    }
}

fn path() -> std::path::PathBuf {
    util::data_dir().join("overlay.json")
}

pub fn settings() -> Settings {
    util::read_json(&path()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

pub fn set_settings(mut s: Settings) {
    s.scale = s.scale.clamp(0.6, 2.0);
    s.opacity = s.opacity.clamp(0.2, 1.0);
    if let Ok(v) = serde_json::to_value(&s) {
        util::write_json(&path(), &v);
        util::emit("ov-config", v);
    }
}

pub fn config_json() -> Value {
    serde_json::to_value(settings()).unwrap_or_default()
}

/// Logical size of the overlay window; the list is drawn in the chosen corner inside it.
const W: f64 = 300.0;
const H: f64 = 440.0;

pub fn start() {
    std::thread::Builder::new()
        .name("vc-overlay".into())
        .spawn(|| {
            let mut last: Option<(i32, i32, i32, i32, bool)> = None;
            loop {
                std::thread::sleep(Duration::from_millis(400));
                let s = settings();
                let want = if s.enabled && crate::session::connected() { imp::target(&s) } else { None };
                match want {
                    Some(r) => {
                        let place = (r.0, r.1, r.2, r.3, s.hide_from_capture);
                        if !ensure() {
                            continue;
                        }
                        if last != Some(place) {
                            last = Some(place);
                            imp::place(r, s.hide_from_capture);
                        } else {
                            imp::top();
                        }
                    }
                    None => {
                        if last.is_some() {
                            last = None;
                            close();
                        }
                    }
                }
            }
        })
        .ok();
}

fn ensure() -> bool {
    let app = util::app();
    if app.get_webview_window("overlay").is_some() {
        return imp::hwnd() != 0;
    }
    let built = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("overlay.html".into()))
        .title("EZ VoiceChat overlay")
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(false)
        .visible(false)
        .inner_size(W, H)
        .build();
    match built {
        Ok(w) => {
            let _ = w.set_ignore_cursor_events(true);
            #[cfg(windows)]
            if let Ok(h) = w.hwnd() {
                imp::adopt(h.0 as isize);
            }
            true
        }
        Err(e) => {
            util::log(format!("overlay window failed: {e}"));
            false
        }
    }
}

fn close() {
    imp::forget();
    if let Some(w) = util::app().get_webview_window("overlay") {
        let _ = w.destroy();
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::atomic::{AtomicIsize, Ordering};
    use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    static HWND_OV: AtomicIsize = AtomicIsize::new(0);

    pub fn hwnd() -> isize {
        HWND_OV.load(Ordering::Acquire)
    }

    pub fn adopt(h: isize) {
        unsafe {
            let w = h as HWND;
            let ex = GetWindowLongPtrW(w, GWL_EXSTYLE);
            let add = (WS_EX_TRANSPARENT | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST) as isize;
            SetWindowLongPtrW(w, GWL_EXSTYLE, (ex | add) & !(WS_EX_APPWINDOW as isize));
        }
        HWND_OV.store(h, Ordering::Release);
    }

    pub fn forget() {
        HWND_OV.store(0, Ordering::Release);
    }

    fn monitor_rect(m: windows_sys::Win32::Graphics::Gdi::HMONITOR) -> Option<RECT> {
        unsafe {
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            (GetMonitorInfoW(m, &mut mi) != 0).then_some(mi.rcMonitor)
        }
    }

    fn class_name(w: HWND) -> String {
        let mut buf = [0u16; 64];
        let n = unsafe { GetClassNameW(w, buf.as_mut_ptr(), buf.len() as i32) };
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }

    /// Where the overlay window goes (physical pixels), or None to hide it.
    pub fn target(s: &Settings) -> Option<(i32, i32, i32, i32)> {
        unsafe {
            let fg = GetForegroundWindow();
            let (mon, dpi) = if s.only_fullscreen {
                if fg.is_null() || fg as isize == hwnd() {
                    return None;
                }
                // Our own window, the desktop and the taskbar don't count as a game.
                let mut pid = 0u32;
                GetWindowThreadProcessId(fg, &mut pid);
                if pid == std::process::id() {
                    return None;
                }
                let cls = class_name(fg);
                if matches!(cls.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
                    return None;
                }
                let mut r: RECT = std::mem::zeroed();
                if GetWindowRect(fg, &mut r) == 0 {
                    return None;
                }
                let m = monitor_rect(MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY))?;
                let covers = r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom;
                if !covers {
                    return None;
                }
                (m, GetDpiForWindow(fg))
            } else {
                let m = monitor_rect(MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY))?;
                (m, 0)
            };
            let k = if dpi > 0 { dpi as f64 / 96.0 } else { 1.0 };
            let (w, h) = ((W * k) as i32, (H * k) as i32);
            let pad = (16.0 * k) as i32;
            let x = if s.corner.ends_with('r') { mon.right - w - pad } else { mon.left + pad };
            let y = if s.corner.starts_with('b') { mon.bottom - h - pad } else { mon.top + pad };
            Some((x, y, w, h))
        }
    }

    pub fn place(r: (i32, i32, i32, i32), hide_capture: bool) {
        let h = hwnd();
        if h == 0 {
            return;
        }
        unsafe {
            let w = h as HWND;
            // WDA_EXCLUDEFROMCAPTURE (Windows 10 2004+) / WDA_NONE
            SetWindowDisplayAffinity(w, if hide_capture { 0x11 } else { 0 });
            SetWindowPos(w, HWND_TOPMOST, r.0, r.1, r.2, r.3, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            ShowWindow(w, SW_SHOWNOACTIVATE);
        }
    }

    /// Games sometimes push themselves above topmost windows; put the overlay back.
    pub fn top() {
        let h = hwnd();
        if h != 0 {
            unsafe {
                SetWindowPos(h as HWND, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
            }
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Settings;
    pub fn hwnd() -> isize {
        0
    }
    pub fn forget() {}
    pub fn target(_s: &Settings) -> Option<(i32, i32, i32, i32)> {
        None
    }
    pub fn place(_r: (i32, i32, i32, i32), _hide: bool) {}
    pub fn top() {}
}
