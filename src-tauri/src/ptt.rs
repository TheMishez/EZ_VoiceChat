//! Push-to-talk through Raw Input: we get a copy of the key/button after the game already has
//! it, so holding your voice key never adds input lag and the key still works in game. Nothing
//! is registered unless push-to-talk is on.
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};

static HELD: AtomicBool = AtomicBool::new(false);
static WANT: Mutex<Option<Key>> = parking_lot::const_mutex(None);
static HWND: AtomicIsize = AtomicIsize::new(0);
/// While the page is recording a key, it reports every key here instead.
static CAPTURE: AtomicBool = AtomicBool::new(false);
static LAST_CODE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy, PartialEq, Debug)]
enum Key {
    Vk(u8),
    Mouse(u8),
}

fn parse(code: &str) -> Option<Key> {
    if let Some(n) = code.strip_prefix("Mouse") {
        return n.parse::<u8>().ok().filter(|n| (1..=5).contains(n)).map(Key::Mouse);
    }
    crate::keys::parse(code).map(|c| Key::Vk(c.vk))
}

pub fn held() -> bool {
    HELD.load(Ordering::Relaxed)
}

pub fn configure(enabled: bool, code: &str) {
    let k = if enabled { parse(code) } else { None };
    *WANT.lock() = k;
    HELD.store(false, Ordering::Relaxed);
    reregister();
}

pub fn set_capture(on: bool) {
    CAPTURE.store(on, Ordering::Relaxed);
    LAST_CODE.store(0, Ordering::Relaxed);
    reregister();
}

fn reregister() {
    #[cfg(windows)]
    {
        let h = HWND.load(Ordering::Acquire);
        if h != 0 {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(h as _, imp::MSG_REREGISTER, 0, 0);
            }
        }
    }
}

pub fn start() {
    #[cfg(windows)]
    std::thread::Builder::new().name("vc-ptt".into()).spawn(imp::main).ok();
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::*;
    use windows_sys::Win32::UI::Input::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    pub const MSG_REREGISTER: u32 = WM_APP + 1;
    static REG: AtomicU32 = AtomicU32::new(0);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn main() {
        unsafe {
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
            let inst = GetModuleHandleW(null());
            let class = wide("EZVoiceChatPtt");
            let mut wc: WNDCLASSEXW = std::mem::zeroed();
            wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
            wc.lpfnWndProc = Some(wndproc);
            wc.hInstance = inst;
            wc.lpszClassName = class.as_ptr();
            RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 0, 0, HWND_MESSAGE, null_mut(), inst, null());
            if hwnd.is_null() {
                return;
            }
            HWND.store(hwnd as isize, Ordering::Release);
            apply(hwnd);
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe fn apply(hwnd: HWND) {
        let want = *WANT.lock();
        let capture = CAPTURE.load(Ordering::Relaxed);
        let kb = capture || matches!(want, Some(Key::Vk(_)));
        let mouse = capture || matches!(want, Some(Key::Mouse(_)));
        let cur = REG.load(Ordering::Relaxed);
        let set = |usage: u16, on: bool| {
            let dev = RAWINPUTDEVICE {
                usUsagePage: 0x01,
                usUsage: usage,
                dwFlags: if on { RIDEV_INPUTSINK } else { RIDEV_REMOVE },
                hwndTarget: if on { hwnd } else { null_mut() },
            };
            RegisterRawInputDevices(&dev, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32);
        };
        if kb != (cur & 1 != 0) {
            set(0x06, kb);
        }
        if mouse != (cur & 2 != 0) {
            set(0x02, mouse);
        }
        REG.store(kb as u32 | ((mouse as u32) << 1), Ordering::Relaxed);
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == MSG_REREGISTER {
            apply(hwnd);
            return 0;
        }
        if msg == WM_INPUT {
            handle(lp);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }

    unsafe fn handle(lp: LPARAM) {
        let mut buf = [0u64; 16];
        let mut size = std::mem::size_of_val(&buf) as u32;
        let got = GetRawInputData(lp as HRAWINPUT, RID_INPUT, buf.as_mut_ptr() as *mut _, &mut size, std::mem::size_of::<RAWINPUTHEADER>() as u32);
        if got == u32::MAX || (got as usize) < std::mem::size_of::<RAWINPUTHEADER>() {
            return;
        }
        let ri = &*(buf.as_ptr() as *const RAWINPUT);
        let want = *WANT.lock();
        let capture = CAPTURE.load(Ordering::Relaxed);
        if ri.header.dwType == RIM_TYPEMOUSE {
            let f = ri.data.mouse.Anonymous.Anonymous.usButtonFlags as u32;
            if f == 0 {
                return; // movement
            }
            const DOWN: [u32; 5] = [0x001, 0x004, 0x010, 0x040, 0x100];
            const UP: [u32; 5] = [0x002, 0x008, 0x020, 0x080, 0x200];
            for i in 0..5u8 {
                let (d, u) = (f & DOWN[i as usize] != 0, f & UP[i as usize] != 0);
                if capture && d && i >= 2 {
                    LAST_CODE.store(0x1000 | (i as u32 + 1), Ordering::Relaxed);
                }
                if want == Some(Key::Mouse(i + 1)) {
                    if d {
                        HELD.store(true, Ordering::Relaxed);
                    }
                    if u {
                        HELD.store(false, Ordering::Relaxed);
                    }
                }
            }
        } else if ri.header.dwType == RIM_TYPEKEYBOARD {
            let k = &ri.data.keyboard;
            if k.VKey == 0xFF {
                return;
            }
            let flags = k.Flags as u32;
            let e0 = flags & RI_KEY_E0 != 0;
            let down = flags & RI_KEY_BREAK == 0;
            let mut vk = k.VKey as u8;
            match vk {
                0x10 => vk = if k.MakeCode == 0x36 { 0xA1 } else { 0xA0 },
                0x11 => vk = if e0 { 0xA3 } else { 0xA2 },
                0x12 => vk = if e0 { 0xA5 } else { 0xA4 },
                _ => {}
            }
            if let Some(Key::Vk(w)) = want {
                // A generic Ctrl/Shift/Alt binding matches either side.
                let same = w == vk
                    || (w == 0x10 && (vk == 0xA0 || vk == 0xA1))
                    || (w == 0x11 && (vk == 0xA2 || vk == 0xA3))
                    || (w == 0x12 && (vk == 0xA4 || vk == 0xA5));
                if same {
                    HELD.store(down, Ordering::Relaxed);
                }
            }
            if capture && down {
                LAST_CODE.store(vk as u32, Ordering::Relaxed);
            }
        }
    }
}
