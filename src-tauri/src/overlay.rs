//! In-game overlay: who's talking, in a corner of the screen, drawn natively.
//!
//! It's one plain layered Win32 window (no browser), sized to its content and redrawn only when
//! something visible changes (someone starts or stops talking, joins, mutes...). While you're
//! not connected, or no fullscreen game is in front, the window doesn't exist at all.
//! Games must run in borderless / "fullscreen windowed" mode for any overlay to show on top.
use crate::util;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

static CFG: Mutex<Option<Settings>> = parking_lot::const_mutex(None);

pub fn settings() -> Settings {
    CFG.lock().get_or_insert_with(|| util::read_json(&path()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()).clone()
}

pub fn set_settings(mut s: Settings) {
    s.scale = s.scale.clamp(0.6, 2.0);
    s.opacity = s.opacity.clamp(0.2, 1.0);
    if let Ok(v) = serde_json::to_value(&s) {
        util::write_json(&path(), &v);
    }
    *CFG.lock() = Some(s);
}

pub fn config_json() -> Value {
    serde_json::to_value(settings()).unwrap_or_default()
}

pub fn start() {
    #[cfg(windows)]
    std::thread::Builder::new().name("vc-overlay".into()).spawn(imp::main).ok();
}

/// One line in the overlay.
#[derive(Clone, PartialEq, Debug)]
struct Row {
    name: String,
    avatar: String,
    talking: bool,
    muted: bool,
    me: bool,
}

/// Everything the overlay shows; it's only redrawn when this changes.
#[derive(Clone, PartialEq, Debug, Default)]
struct Content {
    warn: Option<&'static str>,
    rows: Vec<Row>,
}

fn content(s: &Settings) -> Content {
    let (mute, deaf) = crate::audio::self_state();
    let warn = if deaf {
        Some("Deafened")
    } else if mute {
        Some("Mic muted")
    } else {
        None
    };
    let talking = crate::audio::source().map(|x| x.talking()).unwrap_or_default();
    let me_talking = crate::audio::transmitting();
    let mut rows = crate::session::with_view(|v| {
        let me = v.me?;
        let ch = v.users.get(&me)?.channel;
        Some(
            v.users
                .values()
                .filter(|u| u.channel == ch)
                .map(|u| Row {
                    name: u.name.clone(),
                    avatar: u.avatar.clone(),
                    talking: if u.id == me { me_talking } else { talking.contains(&u.id) },
                    muted: u.self_mute || u.muted || u.self_deaf || u.deafened,
                    me: u.id == me,
                })
                .filter(|r| s.show == "channel" || r.talking)
                .collect::<Vec<_>>(),
        )
    })
    .flatten()
    .unwrap_or_default();
    rows.sort_by(|a, b| b.talking.cmp(&a.talking).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    rows.truncate(12);
    Content { warn, rows }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::collections::HashMap;
    use std::ptr::{null, null_mut};
    use std::time::{Duration, Instant};
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if m == WM_NCHITTEST {
            return -1; // HTTRANSPARENT: clicks go to whatever is underneath
        }
        DefWindowProcW(h, m, w, l)
    }

    /// Placement: (monitor rect, dpi).
    unsafe fn target(s: &Settings, own: HWND) -> Option<(RECT, u32)> {
        let mi = |m: HMONITOR| {
            let mut i: MONITORINFO = std::mem::zeroed();
            i.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            (GetMonitorInfoW(m, &mut i) != 0).then_some(i.rcMonitor)
        };
        if !s.only_fullscreen {
            let m = mi(MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY))?;
            return Some((m, GetDpiForSystem()));
        }
        let fg = GetForegroundWindow();
        if fg.is_null() || fg == own {
            return None;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(fg, &mut pid);
        if pid == std::process::id() {
            return None;
        }
        let mut buf = [0u16; 64];
        let n = GetClassNameW(fg, buf.as_mut_ptr(), buf.len() as i32);
        let cls = String::from_utf16_lossy(&buf[..n.max(0) as usize]);
        if matches!(cls.as_str(), "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd") {
            return None;
        }
        let mut r: RECT = std::mem::zeroed();
        if GetWindowRect(fg, &mut r) == 0 {
            return None;
        }
        let m = mi(MonitorFromWindow(fg, MONITOR_DEFAULTTOPRIMARY))?;
        (r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom).then(|| (m, GetDpiForWindow(fg).max(96)))
    }

    pub fn main() {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_APARTMENTTHREADED);
            let inst = GetModuleHandleW(null());
            let class = wide("EZVoiceChatOverlay");
            let mut wc: WNDCLASSEXW = std::mem::zeroed();
            wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = inst;
            wc.lpszClassName = class.as_ptr();
            RegisterClassExW(&wc);
            let mut ov = Overlay { hwnd: null_mut(), inst, class, drawn: None, pics: HashMap::new(), last_top: Instant::now() };
            loop {
                let mut msg: MSG = std::mem::zeroed();
                while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                let s = settings();
                let place = if s.enabled && crate::session::connected() { target(&s, ov.hwnd) } else { None };
                match place {
                    Some((mon, dpi)) => {
                        let c = content(&s);
                        ov.show(&s, mon, dpi, c);
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    None => {
                        ov.destroy();
                        std::thread::sleep(Duration::from_millis(400));
                    }
                }
            }
        }
    }

    struct Overlay {
        hwnd: HWND,
        inst: HINSTANCE,
        class: Vec<u16>,
        /// What's on screen now: content, settings, monitor, dpi.
        drawn: Option<(Content, Settings, (i32, i32, i32, i32), u32)>,
        /// Decoded avatars: (hash, size px) -> premultiplied RGBA.
        pics: HashMap<(String, u32), Option<Vec<[f32; 4]>>>,
        last_top: Instant,
    }

    impl Overlay {
        unsafe fn destroy(&mut self) {
            if !self.hwnd.is_null() {
                DestroyWindow(self.hwnd);
                self.hwnd = null_mut();
            }
            self.drawn = None;
            self.pics.clear();
        }

        unsafe fn show(&mut self, s: &Settings, mon: RECT, dpi: u32, c: Content) {
            let key = (c, s.clone(), (mon.left, mon.top, mon.right, mon.bottom), dpi);
            if self.drawn.as_ref() == Some(&key) {
                // Unchanged: just make sure the game hasn't pushed itself above us.
                if !self.hwnd.is_null() && self.last_top.elapsed() > Duration::from_secs(1) {
                    self.last_top = Instant::now();
                    SetWindowPos(self.hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
                }
                return;
            }
            if self.hwnd.is_null() {
                let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;
                self.hwnd = CreateWindowExW(ex, self.class.as_ptr(), self.class.as_ptr(), WS_POPUP, 0, 0, 1, 1, null_mut(), null_mut(), self.inst, null());
                if self.hwnd.is_null() {
                    return;
                }
            }
            SetWindowDisplayAffinity(self.hwnd, if s.hide_from_capture { 0x11 } else { 0 });
            let k = dpi as f32 / 96.0 * s.scale;
            match render(&key.0, k, s.corner.ends_with('r'), &mut self.pics) {
                Some(img) => {
                    let pad = (16.0 * dpi as f32 / 96.0) as i32;
                    let x = if s.corner.ends_with('r') { mon.right - img.w - pad } else { mon.left + pad };
                    let y = if s.corner.starts_with('b') { mon.bottom - img.h - pad } else { mon.top + pad };
                    present(self.hwnd, &img, x, y, (s.opacity.clamp(0.2, 1.0) * 255.0) as u8);
                }
                None => {
                    ShowWindow(self.hwnd, SW_HIDE);
                }
            }
            self.drawn = Some(key);
        }
    }

    // ------------------------------------------------------------------ drawing

    pub struct Img {
        w: i32,
        h: i32,
        px: Vec<[f32; 4]>, // premultiplied RGBA, 0..1
    }

    impl Img {
        fn over(&mut self, x: i32, y: i32, c: [f32; 4], cov: f32) {
            if cov <= 0.0 || x < 0 || y < 0 || x >= self.w || y >= self.h {
                return;
            }
            let a = c[3] * cov.min(1.0);
            let p = &mut self.px[(y * self.w + x) as usize];
            for i in 0..3 {
                p[i] = c[i] * a + p[i] * (1.0 - a);
            }
            p[3] = a + p[3] * (1.0 - a);
        }
        fn over_pm(&mut self, x: i32, y: i32, c: [f32; 4], cov: f32) {
            if cov <= 0.0 || x < 0 || y < 0 || x >= self.w || y >= self.h {
                return;
            }
            let p = &mut self.px[(y * self.w + x) as usize];
            let a = c[3] * cov;
            for i in 0..3 {
                p[i] = c[i] * cov + p[i] * (1.0 - a);
            }
            p[3] = a + p[3] * (1.0 - a);
        }
    }

    fn rgba(hex: u32, a: f32) -> [f32; 4] {
        [((hex >> 16) & 255) as f32 / 255.0, ((hex >> 8) & 255) as f32 / 255.0, (hex & 255) as f32 / 255.0, a]
    }

    /// A horizontal capsule (pill): fill plus an optional border, anti-aliased.
    fn pill(img: &mut Img, x: f32, y: f32, w: f32, h: f32, fill: [f32; 4], border: [f32; 4], bw: f32) {
        let r = h / 2.0;
        for py in y.floor() as i32..(y + h).ceil() as i32 {
            for px in x.floor() as i32..(x + w).ceil() as i32 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let cx = fx.clamp(x + r, x + w - r);
                let d = ((fx - cx).powi(2) + (fy - (y + r)).powi(2)).sqrt() - r;
                let inside = (0.5 - d).clamp(0.0, 1.0);
                img.over(px, py, fill, inside);
                if bw > 0.0 {
                    let ring = inside - (0.5 - (d + bw)).clamp(0.0, 1.0);
                    img.over(px, py, border, ring);
                }
            }
        }
    }

    fn circle(img: &mut Img, cx: f32, cy: f32, r: f32, c: [f32; 4]) {
        for py in (cy - r - 1.0) as i32..(cy + r + 1.0) as i32 {
            for px in (cx - r - 1.0) as i32..(cx + r + 1.0) as i32 {
                let d = ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt() - r;
                img.over(px, py, c, (0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    fn ring(img: &mut Img, cx: f32, cy: f32, r: f32, w: f32, c: [f32; 4]) {
        for py in (cy - r - w - 1.0) as i32..(cy + r + w + 1.0) as i32 {
            for px in (cx - r - w - 1.0) as i32..(cx + r + w + 1.0) as i32 {
                let d = ((px as f32 + 0.5 - cx).powi(2) + (py as f32 + 0.5 - cy).powi(2)).sqrt();
                let cov = (0.5 - (d - r - w / 2.0).abs() + w / 2.0).clamp(0.0, 1.0);
                img.over(px, py, c, cov);
            }
        }
    }

    fn line(img: &mut Img, a: (f32, f32), b: (f32, f32), w: f32, c: [f32; 4]) {
        let (x0, x1) = (a.0.min(b.0) - w, a.0.max(b.0) + w);
        let (y0, y1) = (a.1.min(b.1) - w, a.1.max(b.1) + w);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        for py in y0 as i32..y1.ceil() as i32 {
            for px in x0 as i32..x1.ceil() as i32 {
                let (fx, fy) = (px as f32 + 0.5, py as f32 + 0.5);
                let t = (((fx - a.0) * dx + (fy - a.1) * dy) / len2).clamp(0.0, 1.0);
                let d = ((fx - a.0 - t * dx).powi(2) + (fy - a.1 - t * dy).powi(2)).sqrt();
                img.over(px, py, c, (w / 2.0 + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    /// The "muted" mark: a red circle with a slash.
    fn muted_icon(img: &mut Img, cx: f32, cy: f32, size: f32) {
        let red = rgba(0xff4f62, 1.0);
        let r = size / 2.0 - 1.0;
        let w = (size / 7.0).max(1.2);
        ring(img, cx, cy, r - w / 2.0, w, red);
        let o = (r - w) * std::f32::consts::FRAC_1_SQRT_2;
        line(img, (cx - o, cy - o), (cx + o, cy + o), w, red);
    }

    /// Text is drawn by GDI (white on black, greyscale anti-aliasing) into a mask, then
    /// coloured in: GDI can't write alpha itself.
    struct TextJob {
        text: Vec<u16>,
        rect: RECT,
        /// 0 = name, 1 = initials (small, centred), 2 = warning
        font: u8,
        color: [f32; 4],
    }

    unsafe fn font(px: i32, bold: bool) -> HFONT {
        let face = wide(if bold { "Segoe UI Semibold" } else { "Segoe UI" });
        CreateFontW(-px, 0, 0, 0, if bold { 600 } else { 400 }, 0, 0, 0, DEFAULT_CHARSET as _, OUT_DEFAULT_PRECIS as _, CLIP_DEFAULT_PRECIS as _, ANTIALIASED_QUALITY as _, 0, face.as_ptr())
    }

    unsafe fn text_width(dc: HDC, f: HFONT, t: &[u16]) -> i32 {
        let old = SelectObject(dc, f as _);
        let mut sz: SIZE = std::mem::zeroed();
        GetTextExtentPoint32W(dc, t.as_ptr(), t.len() as i32, &mut sz);
        SelectObject(dc, old);
        sz.cx
    }

    fn initials(n: &str) -> String {
        let s: String = n.chars().filter(|c| c.is_alphanumeric()).take(2).collect::<String>().to_uppercase();
        if s.is_empty() {
            "?".into()
        } else {
            s
        }
    }

    unsafe fn render(c: &Content, k: f32, right: bool, pics: &mut HashMap<(String, u32), Option<Vec<[f32; 4]>>>) -> Option<Img> {
        if c.rows.is_empty() && c.warn.is_none() {
            return None;
        }
        let rh = (32.0 * k).round();
        let gap = (6.0 * k).round();
        let av = (24.0 * k).round();
        let icon = (14.0 * k).round();
        let max_text = (190.0 * k) as i32;
        let screen = GetDC(null_mut());
        let dc = CreateCompatibleDC(screen);
        let f_name = font((14.0 * k) as i32, true);
        let f_small = font((10.0 * k) as i32, true);
        let f_warn = font((12.0 * k) as i32, true);

        // Layout: (row width, text width) per line; the warning goes first.
        struct L {
            w: f32,
            tw: i32,
            text: Vec<u16>,
        }
        let mut lines: Vec<L> = Vec::new();
        if let Some(wt) = c.warn {
            let t: Vec<u16> = wt.encode_utf16().collect();
            let tw = text_width(dc, f_warn, &t);
            lines.push(L { w: 12.0 * k + icon + 6.0 * k + tw as f32 + 12.0 * k, tw, text: t });
        }
        for r in &c.rows {
            let t: Vec<u16> = r.name.encode_utf16().collect();
            let tw = text_width(dc, f_name, &t).min(max_text);
            let extra = if r.muted { 6.0 * k + icon } else { 0.0 };
            lines.push(L { w: 4.0 * k + av + 8.0 * k + tw as f32 + extra + 12.0 * k, tw, text: t });
        }
        let ring_pad = (3.0 * k).ceil();
        let w = (lines.iter().map(|l| l.w).fold(0.0, f32::max) + ring_pad * 2.0).ceil() as i32;
        let h = (lines.len() as f32 * (rh + gap) - gap + ring_pad * 2.0).ceil() as i32;
        let mut img = Img { w, h, px: vec![[0.0; 4]; (w * h) as usize] };
        let mut jobs: Vec<TextJob> = Vec::new();
        let mut avatars: Vec<(String, f32, f32)> = Vec::new();

        let bg = rgba(0x08090b, 0.74);
        let mut y = ring_pad;
        let mut li = 0;
        if c.warn.is_some() {
            let l = &lines[0];
            let x = if right { w as f32 - ring_pad - l.w } else { ring_pad };
            pill(&mut img, x, y, l.w, rh, bg, rgba(0xff4f62, 0.55), k.max(1.0));
            muted_icon(&mut img, x + 12.0 * k + icon / 2.0, y + rh / 2.0, icon);
            let tx = (x + 12.0 * k + icon + 6.0 * k) as i32;
            jobs.push(TextJob { text: l.text.clone(), rect: RECT { left: tx, top: y as i32, right: tx + l.tw + 2, bottom: (y + rh) as i32 }, font: 2, color: rgba(0xff8a96, 1.0) });
            y += rh + gap;
            li = 1;
        }
        for r in &c.rows {
            let l = &lines[li];
            li += 1;
            let x = if right { w as f32 - ring_pad - l.w } else { ring_pad };
            let border = if r.talking { rgba(0x39e07a, 0.5) } else { rgba(0xffffff, 0.08) };
            let fill = if r.talking { bg } else { rgba(0x08090b, 0.55) };
            pill(&mut img, x, y, l.w, rh, fill, border, k.max(1.0));
            let (acx, acy) = (x + 4.0 * k + av / 2.0, y + rh / 2.0);
            circle(&mut img, acx, acy, av / 2.0, rgba(0x2a2d34, 1.0));
            let has_pic = !r.avatar.is_empty()
                && pics.entry((r.avatar.clone(), av as u32)).or_insert_with(|| crate::avatar::bytes(&r.avatar).and_then(|b| decode(&b, av as u32))).is_some();
            if !has_pic {
                let t: Vec<u16> = initials(&r.name).encode_utf16().collect();
                let (ax, ay) = ((acx - av / 2.0) as i32, (acy - av / 2.0) as i32);
                jobs.push(TextJob { text: t, rect: RECT { left: ax, top: ay, right: ax + av as i32, bottom: ay + av as i32 }, font: 1, color: rgba(0xdddddd, 1.0) });
            } else {
                avatars.push((r.avatar.clone(), acx, acy));
            }
            if r.talking {
                ring(&mut img, acx, acy, av / 2.0 + k, 2.0 * k, rgba(0x39e07a, 1.0));
            }
            let tx = (x + 4.0 * k + av + 8.0 * k) as i32;
            let name_col = if r.me { rgba(0xffd21f, 1.0) } else if r.talking { rgba(0xffffff, 1.0) } else { rgba(0xc8cad0, 1.0) };
            jobs.push(TextJob { text: l.text.clone(), rect: RECT { left: tx, top: y as i32, right: tx + l.tw + 2, bottom: (y + rh) as i32 }, font: 0, color: name_col });
            if r.muted {
                muted_icon(&mut img, tx as f32 + l.tw as f32 + 6.0 * k + icon / 2.0, y + rh / 2.0, icon);
            }
            y += rh + gap;
        }

        // Pictures.
        let size = av as u32;
        for (hash, cx, cy) in avatars {
            if let Some(Some(p)) = pics.get(&(hash, size)) {
                let (x0, y0) = (cx - av / 2.0, cy - av / 2.0);
                for py in 0..size as i32 {
                    for px in 0..size as i32 {
                        let d = ((px as f32 + 0.5 - av / 2.0).powi(2) + (py as f32 + 0.5 - av / 2.0).powi(2)).sqrt() - av / 2.0;
                        let cov = (0.5 - d).clamp(0.0, 1.0);
                        img.over_pm(x0 as i32 + px, y0 as i32 + py, p[(py * size as i32 + px) as usize], cov);
                    }
                }
            }
        }

        // Text through a mask.
        if !jobs.is_empty() {
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = w;
            bi.bmiHeader.biHeight = -h;
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            let mut bits: *mut core::ffi::c_void = null_mut();
            let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if !bmp.is_null() && !bits.is_null() {
                let old = SelectObject(dc, bmp as _);
                SetBkMode(dc, TRANSPARENT as _);
                SetTextColor(dc, 0x00FF_FFFF);
                for j in &jobs {
                    let f = match j.font {
                        1 => f_small,
                        2 => f_warn,
                        _ => f_name,
                    };
                    let of = SelectObject(dc, f as _);
                    let mut rc = j.rect;
                    let fmt = DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS | if j.font == 1 { DT_CENTER } else { DT_LEFT };
                    DrawTextW(dc, j.text.as_ptr() as _, j.text.len() as i32, &mut rc, fmt);
                    SelectObject(dc, of);
                }
                GdiFlush();
                let mask = std::slice::from_raw_parts(bits as *const u32, (w * h) as usize);
                for j in &jobs {
                    for py in j.rect.top.max(0)..j.rect.bottom.min(h) {
                        for px in j.rect.left.max(0)..j.rect.right.min(w) {
                            let v = mask[(py * w + px) as usize];
                            let cov = ((v >> 8) & 255) as f32 / 255.0;
                            img.over(px, py, j.color, cov);
                        }
                    }
                }
                SelectObject(dc, old);
                DeleteObject(bmp as _);
            }
        }
        DeleteObject(f_name as _);
        DeleteObject(f_small as _);
        DeleteObject(f_warn as _);
        DeleteDC(dc);
        ReleaseDC(null_mut(), screen);
        Some(img)
    }

    unsafe fn present(hwnd: HWND, img: &Img, x: i32, y: i32, alpha: u8) {
        let screen = GetDC(null_mut());
        let dc = CreateCompatibleDC(screen);
        let mut bi: BITMAPINFO = std::mem::zeroed();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = img.w;
        bi.bmiHeader.biHeight = -img.h;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        let mut bits: *mut core::ffi::c_void = null_mut();
        let bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
        if !bmp.is_null() && !bits.is_null() {
            let out = std::slice::from_raw_parts_mut(bits as *mut u32, (img.w * img.h) as usize);
            for (o, p) in out.iter_mut().zip(img.px.iter()) {
                let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
                *o = (c(p[3]) << 24) | (c(p[0]) << 16) | (c(p[1]) << 8) | c(p[2]);
            }
            let old = SelectObject(dc, bmp as _);
            let pos = POINT { x, y };
            let size = SIZE { cx: img.w, cy: img.h };
            let src = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION { BlendOp: AC_SRC_OVER as u8, BlendFlags: 0, SourceConstantAlpha: alpha, AlphaFormat: AC_SRC_ALPHA as u8 };
            UpdateLayeredWindow(hwnd, screen, &pos, &size, dc, &src, 0, &blend, ULW_ALPHA);
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
            SelectObject(dc, old);
            DeleteObject(bmp as _);
        }
        DeleteDC(dc);
        ReleaseDC(null_mut(), screen);
    }

    /// Decodes a JPEG/PNG/GIF/WebP profile picture with Windows' own image decoders (WIC),
    /// cropped to a centred square and scaled to `size`.
    unsafe fn decode(bytes: &[u8], size: u32) -> Option<Vec<[f32; 4]>> {
        use windows::Win32::Graphics::Imaging::*;
        use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
        let f: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let stream = f.CreateStream().ok()?;
        stream.InitializeFromMemory(bytes).ok()?;
        let dec = f.CreateDecoderFromStream(&stream, null(), WICDecodeMetadataCacheOnDemand).ok()?;
        let frame = dec.GetFrame(0).ok()?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h).ok()?;
        let side = w.min(h).max(1);
        let clip = f.CreateBitmapClipper().ok()?;
        let r = WICRect { X: ((w - side) / 2) as i32, Y: ((h - side) / 2) as i32, Width: side as i32, Height: side as i32 };
        clip.Initialize(&frame, &r).ok()?;
        let sc = f.CreateBitmapScaler().ok()?;
        sc.Initialize(&clip, size, size, WICBitmapInterpolationModeFant).ok()?;
        let conv = f.CreateFormatConverter().ok()?;
        conv.Initialize(&sc, &GUID_WICPixelFormat32bppPBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom).ok()?;
        let mut buf = vec![0u8; (size * size * 4) as usize];
        conv.CopyPixels(null(), size * 4, &mut buf).ok()?;
        Some(buf.chunks_exact(4).map(|p| [p[2] as f32 / 255.0, p[1] as f32 / 255.0, p[0] as f32 / 255.0, p[3] as f32 / 255.0]).collect())
    }
}
