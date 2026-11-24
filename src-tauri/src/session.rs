//! What the UI sees, whichever kind of server we're on (Mumble or TeamSpeak 3).
//!
//! Each protocol keeps a `View` up to date and calls `publish()`; the page renders it. Voice goes
//! through the shared audio core via `VoiceSink` (mic -> server) and `VoiceSource` (server ->
//! speakers), so the audio path is the same for every protocol.
use parking_lot::Mutex;
use serde::Serialize;
use serde_json::json;
use std::collections::BTreeMap;
use tokio::sync::mpsc;

#[derive(Serialize, Clone, Default, Debug)]
pub struct Channel {
    pub id: u64,
    pub parent: Option<u64>,
    pub name: String,
    pub position: i64,
    pub description: String,
    pub temporary: bool,
}

#[derive(Serialize, Clone, Default, Debug)]
pub struct User {
    pub id: u64,
    pub name: String,
    pub channel: u64,
    pub muted: bool,
    pub deafened: bool,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub registered: bool,
    pub comment: String,
}

#[derive(Serialize, Clone, Default, Debug)]
pub struct View {
    pub kind: String,
    pub host: String,
    pub server_name: String,
    pub welcome: String,
    pub channels: BTreeMap<u64, Channel>,
    pub users: BTreeMap<u64, User>,
    pub me: Option<u64>,
    pub ping_ms: Option<f64>,
    pub udp: bool,
    pub codec: String,
}

/// Commands from the page to the connected server.
#[derive(Debug)]
pub enum Cmd {
    Join(u64),
    Chat(String),
    SelfState { mute: bool, deaf: bool },
    Disconnect,
}

/// Microphone -> server. Called on the audio thread with 48 kHz mono frames.
pub trait VoiceSink: Send {
    /// `last` = the user stopped talking (the frame may be empty).
    fn frame(&mut self, pcm: &[f32], last: bool);
}

/// Server -> speakers. Called on the audio output thread.
pub trait VoiceSource: Send + Sync {
    /// Adds everyone's voice into `out` (interleaved stereo, 48 kHz).
    fn mix(&self, out: &mut [f32]);
    /// Who is audible right now.
    fn talking(&self) -> Vec<u64>;
    fn set_volume(&self, user: u64, volume: f32);
}

static VIEW: Mutex<Option<View>> = parking_lot::const_mutex(None);
static CMD: Mutex<Option<mpsc::UnboundedSender<Cmd>>> = parking_lot::const_mutex(None);
static GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn with_view<R>(f: impl FnOnce(&mut View) -> R) -> Option<R> {
    VIEW.lock().as_mut().map(f)
}

pub fn set_view(v: Option<View>) {
    *VIEW.lock() = v;
    publish();
}

pub fn view_json() -> serde_json::Value {
    match VIEW.lock().as_ref() {
        Some(v) => serde_json::to_value(v).unwrap_or_default(),
        None => serde_json::Value::Null,
    }
}

pub fn publish() {
    crate::util::emit("vc-state", view_json());
    crate::tray_refresh();
}

static LAST_STATUS: Mutex<Option<serde_json::Value>> = parking_lot::const_mutex(None);

pub fn last_status() -> serde_json::Value {
    LAST_STATUS.lock().clone().unwrap_or_else(|| json!({ "phase": "idle", "message": "" }))
}

pub fn status(phase: &str, message: impl Into<String>) {
    let m: String = message.into();
    *LAST_STATUS.lock() = Some(json!({ "phase": phase, "message": m.clone() }));
    if phase == "error" || phase == "disconnected" {
        crate::util::log(format!("{phase}: {m}"));
    }
    crate::util::emit("vc-status", json!({ "phase": phase, "message": m }));
}

pub fn chat(from: &str, text: &str, kind: &str) {
    crate::util::emit("vc-chat", json!({ "from": from, "text": text, "kind": kind, "time": crate::util::now_ms() }));
}

/// Starts a new session generation; the old one's commands channel is replaced.
pub fn begin(tx: mpsc::UnboundedSender<Cmd>) -> u64 {
    *CMD.lock() = Some(tx);
    GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
}

pub fn generation() -> u64 {
    GEN.load(std::sync::atomic::Ordering::SeqCst)
}

/// Called by a session when it ends (only clears state if it's still the current one).
pub fn end(gen: u64) {
    if generation() == gen {
        *CMD.lock() = None;
        crate::audio::set_sink(None);
        crate::audio::set_source(None);
        set_view(None);
    }
}

pub fn send(c: Cmd) -> bool {
    CMD.lock().as_ref().map(|t| t.send(c).is_ok()).unwrap_or(false)
}

pub fn connected() -> bool {
    CMD.lock().is_some()
}
