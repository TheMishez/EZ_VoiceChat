//! Small shared helpers: app handle, events, data folder, JSON files, logging.
use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};

static APP: OnceLock<AppHandle> = OnceLock::new();

pub fn set_app(a: AppHandle) {
    let _ = APP.set(a);
}

pub fn app() -> &'static AppHandle {
    APP.get().expect("app not set")
}

pub fn app_opt() -> Option<&'static AppHandle> {
    APP.get()
}

pub fn emit<S: Serialize + Clone>(event: &str, payload: S) {
    if let Some(a) = APP.get() {
        let _ = a.emit(event, payload);
    }
}

/// %APPDATA%\EZ VoiceChat
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let d = base.join("EZ VoiceChat");
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

pub fn write_json(p: &Path, v: &Value) {
    let tmp = p.with_extension("tmp");
    if let Ok(data) = serde_json::to_vec_pretty(v) {
        if std::fs::write(&tmp, data).is_ok() {
            let _ = std::fs::rename(&tmp, p);
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn log(msg: impl AsRef<str>) {
    use std::io::Write;
    let p = data_dir().join("voicechat.log");
    if std::fs::metadata(&p).map(|m| m.len() > 1 << 20).unwrap_or(false) {
        let _ = std::fs::remove_file(&p);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{} {}", now_ms(), msg.as_ref());
    }
}

/// Mumble sends chat as HTML: keep the text, drop the tags.
pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let s = s.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n").replace("</p>", "\n");
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ").replace("&amp;", "&").trim().to_string()
}

pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
