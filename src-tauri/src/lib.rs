//! EZ VoiceChat: one lightweight client for Mumble and TeamSpeak 3 servers.
//!
//! The window only exists while it's open; closing it keeps you connected from the tray
//! (voice keeps running in Rust, with no browser engine involved).
mod audio;
mod keys;
mod mumble;
mod ptt;
mod session;
mod overlay;
mod ts;
mod updater;
mod util;
#[cfg(windows)]
mod wasapi;

use serde_json::{json, Value};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

// ------------------------------------------------------------------ window + tray

fn show_main() {
    let app = util::app().clone();
    std::thread::spawn(move || {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
            return;
        }
        let r = WebviewWindowBuilder::new(&app, "main", WebviewUrl::App("index.html".into()))
            .title("EZ VoiceChat")
            .inner_size(1180.0, 760.0)
            .min_inner_size(860.0, 560.0)
            .decorations(false)
            .disable_drag_drop_handler()
            .build();
        if let Err(e) = r {
            util::log(format!("window: {e}"));
        }
    });
}

fn build_menu(app: &tauri::AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let (mute, deaf) = audio::self_state();
    let open = MenuItem::with_id(app, "open", "Open EZ VoiceChat", true, None::<&str>)?;
    let m = MenuItem::with_id(app, "mute", if mute { "Unmute microphone" } else { "Mute microphone" }, true, None::<&str>)?;
    let d = MenuItem::with_id(app, "deaf", if deaf { "Undeafen" } else { "Deafen" }, true, None::<&str>)?;
    let disc = MenuItem::with_id(app, "disconnect", "Disconnect", session::connected(), None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let s1 = PredefinedMenuItem::separator(app)?;
    let s2 = PredefinedMenuItem::separator(app)?;
    Menu::with_items(app, &[&open, &s1, &m, &d, &disc, &s2, &quit])
}

pub fn tray_refresh() {
    let Some(app) = util::app_opt() else { return };
    let a = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(t) = a.tray_by_id("main") {
            if let Ok(m) = build_menu(&a) {
                let _ = t.set_menu(Some(m));
            }
            let tip = session::with_view(|v| format!("EZ VoiceChat \u{2013} {}", v.server_name)).unwrap_or_else(|| "EZ VoiceChat".into());
            let _ = t.set_tooltip(Some(tip));
        }
    });
}

fn set_self(mute: bool, deaf: bool) {
    let (m0, d0) = audio::self_state();
    let (m1, d1) = (mute || deaf, deaf);
    if d1 != d0 {
        audio::cue(if d1 { audio::Cue::Deafen } else { audio::Cue::Undeafen });
    } else if m1 != m0 {
        audio::cue(if m1 { audio::Cue::Mute } else { audio::Cue::Unmute });
    }
    audio::set_self(mute, deaf);
    session::send(session::Cmd::SelfState { mute: mute || deaf, deaf });
    util::emit("vc-self", json!({ "mute": mute || deaf, "deaf": deaf }));
    tray_refresh();
}

/// Hotkey: mute / unmute your mic (undeafens too, like the button).
pub fn toggle_mute() {
    let (m, d) = audio::self_state();
    if d {
        set_self(false, false);
    } else {
        set_self(!m, false);
    }
}

/// Hotkey: deafen / undeafen.
pub fn toggle_deaf() {
    let (_, d) = audio::self_state();
    set_self(!d, !d);
}

pub fn quit() {
    session::send(session::Cmd::Disconnect);
    std::thread::sleep(std::time::Duration::from_millis(250));
    util::app().exit(0);
}

// ------------------------------------------------------------------ stored data

fn store_path(name: &str) -> std::path::PathBuf {
    util::data_dir().join(name)
}

// ------------------------------------------------------------------ commands

#[tauri::command]
fn vc_state() -> Value {
    let (mute, deaf) = audio::self_state();
    json!({ "view": session::view_json(), "status": session::last_status(), "self": { "mute": mute, "deaf": deaf }, "version": env!("CARGO_PKG_VERSION") })
}

#[tauri::command]
fn vc_connect(kind: String, host: String, port: Option<u16>, nickname: String, password: Option<String>, trust: Option<String>) -> Result<(), String> {
    let host = host.trim().to_string();
    let nickname = nickname.trim().chars().take(64).collect::<String>();
    if host.is_empty() {
        return Err("Enter a server address".into());
    }
    if nickname.is_empty() {
        return Err("Pick a nickname".into());
    }
    // One server at a time: leave the current one first.
    if session::connected() {
        session::send(session::Cmd::Disconnect);
    }
    let password = password.unwrap_or_default();
    match kind.as_str() {
        "ts3" => ts::connect(ts::Config { host, port: port.unwrap_or(9987), nickname, password }),
        _ => mumble::connect(mumble::Config { host, port: port.unwrap_or(64738), username: nickname, password, trust }),
    }
    Ok(())
}

#[tauri::command]
fn vc_disconnect() {
    if session::send(session::Cmd::Disconnect) {
        session::status("disconnected", "Disconnected");
    }
}

#[tauri::command]
fn vc_join(channel: u64) {
    session::send(session::Cmd::Join(channel));
}

#[tauri::command]
fn vc_chat(text: String) {
    let t: String = text.trim().chars().take(5000).collect();
    if !t.is_empty() {
        session::send(session::Cmd::Chat(t));
    }
}

#[tauri::command]
fn vc_set_self(mute: bool, deaf: bool) {
    set_self(mute, deaf);
}

#[tauri::command]
fn vc_user_volume(user: u64, volume: f32) {
    if let Some(s) = audio::source() {
        s.set_volume(user, volume);
    }
}

#[tauri::command]
fn vc_get_settings() -> audio::Settings {
    audio::settings()
}

#[tauri::command]
fn vc_set_settings(settings: audio::Settings) {
    audio::set_settings(settings);
}

#[tauri::command]
async fn vc_devices() -> Value {
    audio::devices()
}

#[tauri::command]
fn vc_levels() -> Value {
    audio::levels()
}

#[tauri::command]
fn vc_latency() -> Value {
    audio::latency()
}

// ------------------------------------------------------------------ local test server

// ------------------------------------------------------------------ overlay, updates, invites

#[tauri::command]
fn ov_get() -> Value {
    overlay::config_json()
}

#[tauri::command]
fn ov_set(settings: overlay::Settings) {
    overlay::set_settings(settings);
}

#[tauri::command]
fn upd_state() -> Value {
    updater::state()
}

#[tauri::command]
async fn upd_check() -> Value {
    updater::check()
}

#[tauri::command]
fn upd_install() -> bool {
    updater::install()
}

/// An ezvc:// invite the app was started with (or received) that the page hasn't shown yet.
static PENDING_INVITE: parking_lot::Mutex<Option<String>> = parking_lot::const_mutex(None);

#[tauri::command]
fn vc_take_invite() -> Option<String> {
    PENDING_INVITE.lock().take()
}

fn invite_from_args<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    args.into_iter().find(|a| a.to_ascii_lowercase().starts_with("ezvc://")).map(|a| a.chars().take(2000).collect())
}

/// Invites are only ever shown to you to confirm; nothing joins on its own.
fn handle_invite(url: String) {
    *PENDING_INVITE.lock() = Some(url.clone());
    show_main();
    util::emit("vc-invite", url);
}

/// Makes ezvc:// links open this app (per-user, refreshed every start so it follows updates).
#[cfg(windows)]
fn register_scheme() {
    use windows_sys::Win32::System::Registry::*;
    let Ok(exe) = std::env::current_exe() else { return };
    let exe = exe.to_string_lossy().to_string();
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let set = |key: &str, name: &str, value: &str| unsafe {
        let mut h: HKEY = std::ptr::null_mut();
        let k = wide(key);
        if RegCreateKeyExW(HKEY_CURRENT_USER, k.as_ptr(), 0, std::ptr::null(), REG_OPTION_NON_VOLATILE, KEY_WRITE, std::ptr::null(), &mut h, std::ptr::null_mut()) == 0 {
            let n = wide(name);
            let v = wide(value);
            RegSetValueExW(h, if name.is_empty() { std::ptr::null() } else { n.as_ptr() }, 0, REG_SZ, v.as_ptr() as *const u8, (v.len() * 2) as u32);
            RegCloseKey(h);
        }
    };
    set(r"Software\Classes\ezvc", "", "URL:EZ VoiceChat invite");
    set(r"Software\Classes\ezvc", "URL Protocol", "");
    set(r"Software\Classes\ezvc\DefaultIcon", "", &format!("\"{exe}\",0"));
    set(r"Software\Classes\ezvc\shell\open\command", "", &format!("\"{exe}\" \"%1\""));
}

#[tauri::command]
fn app_get_autostart() -> bool {
    use tauri_plugin_autostart::ManagerExt;
    util::app().autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn app_set_autostart(on: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let al = util::app().autolaunch();
    if on { al.enable() } else { al.disable() }.map_err(|e| e.to_string())
}

#[tauri::command]
fn srv_get() -> Value {
    let s = mumble::server::settings();
    json!({
        "settings": s,
        "accepted": s.accepted_terms >= mumble::server::TERMS_VERSION,
        "status": mumble::server::status(),
    })
}

#[tauri::command]
fn srv_accept_terms() {
    mumble::server::accept_terms();
}

#[tauri::command]
fn srv_save(settings: mumble::server::Settings) {
    mumble::server::save(settings);
}

#[tauri::command]
async fn srv_start(settings: mumble::server::Settings) -> Result<Value, String> {
    mumble::server::save(settings);
    mumble::server::start()
}

#[tauri::command]
async fn srv_stop() {
    mumble::server::stop();
}

#[tauri::command]
fn vc_mic_test(on: bool) {
    audio::set_mic_test(on);
}

#[tauri::command]
fn vc_store_get(name: String) -> Value {
    match name.as_str() {
        "bookmarks" | "profile" | "voices" | "app" => util::read_json(&store_path(&format!("{name}.json"))).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

#[tauri::command]
fn vc_store_set(name: String, data: Value) {
    if matches!(name.as_str(), "bookmarks" | "profile" | "voices" | "app") {
        util::write_json(&store_path(&format!("{name}.json")), &data);
    }
}

#[tauri::command]
fn win_min(window: tauri::WebviewWindow) {
    let _ = window.minimize();
}

#[tauri::command]
fn win_max(window: tauri::WebviewWindow) -> bool {
    let m = window.is_maximized().unwrap_or(false);
    let _ = if m { window.unmaximize() } else { window.maximize() };
    !m
}

#[tauri::command]
fn win_close(window: tauri::WebviewWindow) {
    let _ = window.close();
}

#[tauri::command]
fn app_open_external(url: String) {
    use tauri_plugin_opener::OpenerExt;
    if url.starts_with("https://") {
        let _ = util::app().opener().open_url(url, None::<&str>);
    }
}

#[tauri::command]
fn app_quit() {
    quit();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let hidden = std::env::args().any(|a| a == "--hidden");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_a, args, _cwd| match invite_from_args(args) {
            Some(url) => handle_invite(url),
            None => show_main(),
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--hidden"])))
        // Push-to-talk registers its own Raw Input; the window library must not take it over.
        .device_event_filter(tauri::DeviceEventFilter::Always)
        .invoke_handler(tauri::generate_handler![
            vc_state,
            vc_connect,
            vc_disconnect,
            vc_join,
            vc_chat,
            vc_set_self,
            vc_user_volume,
            vc_get_settings,
            vc_set_settings,
            vc_devices,
            vc_levels,
            vc_mic_test,
            vc_store_get,
            vc_store_set,
            win_min,
            win_max,
            win_close,
            app_open_external,
            vc_latency,
            srv_get,
            app_get_autostart,
            ov_get,
            ov_set,
            upd_state,
            upd_check,
            upd_install,
            vc_take_invite,
            app_set_autostart,
            srv_accept_terms,
            srv_save,
            srv_start,
            srv_stop,
            app_quit,
        ])
        .setup(move |app| {
            util::set_app(app.handle().clone());
            std::panic::set_hook(Box::new(|info| util::log(format!("panic: {info}"))));
            ptt::start();
            audio::start();
            overlay::start();
            updater::init();
            #[cfg(windows)]
            std::thread::spawn(register_scheme);
            if let Some(url) = invite_from_args(std::env::args()) {
                *PENDING_INVITE.lock() = Some(url);
            }
            let menu = build_menu(app.handle())?;
            let mut tray = TrayIconBuilder::with_id("main").tooltip("EZ VoiceChat").menu(&menu).show_menu_on_left_click(false);
            if let Ok(i) = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png")) {
                tray = tray.icon(i);
            }
            tray.on_menu_event(|_app, ev| match ev.id().as_ref() {
                "open" => show_main(),
                "mute" => {
                    let (m, d) = audio::self_state();
                    set_self(!m, d && m);
                }
                "deaf" => {
                    let (_, d) = audio::self_state();
                    set_self(!d, !d);
                }
                "disconnect" => vc_disconnect(),
                "quit" => quit(),
                _ => {}
            })
            .on_tray_icon_event(|_t, ev| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                    show_main();
                }
            })
            .build(app)?;
            if !hidden || PENDING_INVITE.lock().is_some() {
                show_main();
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building EZ VoiceChat");
    app.run(|_app, event| {
        if let RunEvent::ExitRequested { api, code, .. } = event {
            // Closing the window keeps you connected from the tray.
            if code.is_none() {
                api.prevent_exit();
            }
        }
    });
}
