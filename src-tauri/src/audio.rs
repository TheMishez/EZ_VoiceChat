//! The shared audio core: microphone in, voices out. Same for Mumble and TeamSpeak.
//!
//!  mic (WASAPI) ─► mono 48 kHz ─► gain ─► noise suppression (RNNoise) ─► voice activation /
//!  push-to-talk gate ─► 10 or 20 ms frames ─► VoiceSink (the protocol encodes Opus and sends)
//!
//!  VoiceSource (decoded, jitter-buffered voices) ─► volume ─► speakers (WASAPI, 48 kHz stereo)
//!
//! Everything runs on WASAPI's audio threads; the callbacks never allocate in the steady state.
use crate::session::{VoiceSink, VoiceSource};
use crate::util;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

pub const RATE: u32 = 48000;

/// Device buffer delays of the open streams (f32 bits, ms) and whether low-latency mode is on.
static IN_MS: AtomicU32 = AtomicU32::new(0);
static OUT_MS: AtomicU32 = AtomicU32::new(0);
static IN_LL: AtomicBool = AtomicBool::new(false);
static OUT_LL: AtomicBool = AtomicBool::new(false);

enum Stream {
    #[allow(dead_code)]
    Cpal(cpal::Stream),
    #[cfg(windows)]
    #[allow(dead_code)]
    Low(crate::wasapi::Stream),
}

fn output_ms() -> f32 {
    #[cfg(windows)]
    if OUT_LL.load(Ordering::Relaxed) {
        return f32::from_bits(crate::wasapi::OUT_QUEUED_MS.load(Ordering::Relaxed));
    }
    f32::from_bits(OUT_MS.load(Ordering::Relaxed))
}

/// Delay estimate, built from measured parts (device buffers, jitter buffer, ping) plus the fixed
/// costs of the codec and noise suppression.
pub fn latency() -> Value {
    let st = settings();
    let frame = st.frame_ms.clamp(10, 20) as f32;
    let (ping, kind) = crate::session::with_view(|v| (v.ping_ms, v.kind.clone())).unwrap_or((None, String::new()));
    json!({
        "input": f32::from_bits(IN_MS.load(Ordering::Relaxed)),
        "output": output_ms(),
        "inputLow": IN_LL.load(Ordering::Relaxed),
        "outputLow": OUT_LL.load(Ordering::Relaxed),
        "frame": frame,
        "codec": 2.5,
        "ns": if st.noise_suppression { 10.0 } else { 0.0 },
        "jitter": source().and_then(|s| s.jitter_ms()),
        "ping": ping,
        "kind": kind,
    })
}
const BLOCK: usize = 480; // 10 ms

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub input_device: String,
    pub output_device: String,
    /// "vad" (voice activation) | "ptt" (push-to-talk) | "continuous"
    pub mode: String,
    pub vad_threshold_db: f32,
    pub ptt_key: String,
    pub ptt_release_ms: u32,
    pub noise_suppression: bool,
    pub input_gain: f32,
    pub output_volume: f32,
    /// Opus frame length: 10 ms = lowest latency, 20 ms = a little more robust
    pub frame_ms: u32,
    pub bitrate: u32,
    /// Ask Windows for its smallest audio period (about 3 ms instead of 10 ms).
    pub low_latency: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            input_device: String::new(),
            output_device: String::new(),
            mode: "vad".into(),
            vad_threshold_db: -42.0,
            ptt_key: String::new(),
            ptt_release_ms: 150,
            noise_suppression: true,
            input_gain: 1.0,
            output_volume: 1.0,
            frame_ms: 10,
            bitrate: 64000,
            low_latency: true,
        }
    }
}

fn settings_path() -> std::path::PathBuf {
    util::data_dir().join("audio.json")
}

pub fn load_settings() -> Settings {
    util::read_json(&settings_path()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

// ------------------------------------------------------------------ shared state

static SETTINGS: Mutex<Option<Settings>> = parking_lot::const_mutex(None);
static SINK: Mutex<Option<Box<dyn VoiceSink>>> = parking_lot::const_mutex(None);
static SOURCE: Mutex<Option<Arc<dyn VoiceSource>>> = parking_lot::const_mutex(None);
static LOOPBACK: Mutex<Option<VecDeque<f32>>> = parking_lot::const_mutex(None);
static SELF_MUTE: AtomicBool = AtomicBool::new(false);
static SELF_DEAF: AtomicBool = AtomicBool::new(false);
static MIC_TEST: AtomicBool = AtomicBool::new(false);
static TRANSMITTING: AtomicBool = AtomicBool::new(false);
static LEVEL_DB: AtomicU32 = AtomicU32::new(0);
static GATE_OPEN: AtomicBool = AtomicBool::new(false);
static CTL: OnceLock<Mutex<mpsc::Sender<()>>> = OnceLock::new();

pub fn settings() -> Settings {
    SETTINGS.lock().clone().unwrap_or_default()
}

pub fn set_settings(s: Settings) {
    let old = SETTINGS.lock().replace(s.clone());
    util::write_json(&settings_path(), &serde_json::to_value(&s).unwrap_or_default());
    crate::ptt::configure(s.mode == "ptt", &s.ptt_key);
    let devices_changed = old.map(|o| o.input_device != s.input_device || o.output_device != s.output_device || o.low_latency != s.low_latency).unwrap_or(true);
    if devices_changed {
        rebuild();
    }
}

pub fn set_sink(s: Option<Box<dyn VoiceSink>>) {
    *SINK.lock() = s;
}

pub fn set_source(s: Option<Arc<dyn VoiceSource>>) {
    *SOURCE.lock() = s;
}

pub fn source() -> Option<Arc<dyn VoiceSource>> {
    SOURCE.lock().clone()
}

pub fn set_self(mute: bool, deaf: bool) {
    SELF_MUTE.store(mute || deaf, Ordering::Relaxed);
    SELF_DEAF.store(deaf, Ordering::Relaxed);
}

pub fn self_state() -> (bool, bool) {
    (SELF_MUTE.load(Ordering::Relaxed), SELF_DEAF.load(Ordering::Relaxed))
}

pub fn set_mic_test(on: bool) {
    MIC_TEST.store(on, Ordering::Relaxed);
    *LOOPBACK.lock() = if on { Some(VecDeque::with_capacity(48000)) } else { None };
}

pub fn transmitting() -> bool {
    TRANSMITTING.load(Ordering::Relaxed)
}

pub fn levels() -> Value {
    json!({
        "db": f32::from_bits(LEVEL_DB.load(Ordering::Relaxed)),
        "gate": GATE_OPEN.load(Ordering::Relaxed),
        "transmitting": transmitting(),
        "ptt": crate::ptt::held(),
    })
}

pub fn rebuild() {
    if let Some(c) = CTL.get() {
        let _ = c.lock().send(());
    }
}

// ------------------------------------------------------------------ devices

fn device_list(input: bool) -> Vec<String> {
    let host = cpal::default_host();
    let it = if input { host.input_devices().ok().map(|d| d.collect::<Vec<_>>()) } else { host.output_devices().ok().map(|d| d.collect::<Vec<_>>()) };
    it.unwrap_or_default().iter().map(|d| d.to_string()).collect()
}

pub fn devices() -> Value {
    let host = cpal::default_host();
    let di = host.default_input_device().map(|d| d.to_string()).unwrap_or_default();
    let dout = host.default_output_device().map(|d| d.to_string()).unwrap_or_default();
    json!({ "inputs": device_list(true), "outputs": device_list(false), "defaultInput": di, "defaultOutput": dout })
}

fn find(input: bool, name: &str) -> Option<cpal::Device> {
    let host = cpal::default_host();
    if !name.is_empty() {
        let list = if input { host.input_devices().ok().map(|d| d.collect::<Vec<_>>()) } else { host.output_devices().ok().map(|d| d.collect::<Vec<_>>()) };
        if let Some(d) = list.unwrap_or_default().into_iter().find(|d| d.to_string() == name) {
            return Some(d);
        }
    }
    if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
}

// ------------------------------------------------------------------ control thread (owns the streams)

pub fn start() {
    *SETTINGS.lock() = Some(load_settings());
    let s = settings();
    crate::ptt::configure(s.mode == "ptt", &s.ptt_key);
    let (tx, rx) = mpsc::channel::<()>();
    let _ = CTL.set(Mutex::new(tx.clone()));
    std::thread::Builder::new()
        .name("vc-audio".into())
        .spawn(move || {
            let mut streams: (Option<Stream>, Option<Stream>) = (None, None);
            loop {
                streams.0 = None;
                streams.1 = None;
                let st = settings();
                match open_input(&st, tx.clone()) {
                    Ok(s) => streams.0 = Some(s),
                    Err(e) => util::log(format!("microphone: {e}")),
                }
                match open_output(&st, tx.clone()) {
                    Ok(s) => streams.1 = Some(s),
                    Err(e) => util::log(format!("speakers: {e}")),
                }
                util::emit("vc-audio", json!({ "input": streams.0.is_some(), "output": streams.1.is_some() }));
                // Wait for a change (or retry a missing device every few seconds).
                let missing = streams.0.is_none() || streams.1.is_none();
                match rx.recv_timeout(if missing { Duration::from_secs(3) } else { Duration::from_secs(3600 * 24) }) {
                    Ok(()) => while rx.try_recv().is_ok() {},
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if !missing {
                            continue;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("audio thread");
    // Talking indicators for the page.
    std::thread::spawn(|| {
        let mut last = String::new();
        loop {
            std::thread::sleep(Duration::from_millis(80));
            let mut ids = source().map(|s| s.talking()).unwrap_or_default();
            ids.sort();
            let me = transmitting();
            let key = format!("{ids:?}{me}");
            if key != last {
                last = key;
                util::emit("vc-talking", json!({ "ids": ids, "me": me }));
            }
        }
    });
}

fn mmcss() {
    #[cfg(windows)]
    {
        thread_local!(static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
        DONE.with(|d| {
            if !d.get() {
                d.set(true);
                unsafe {
                    windows_sys::Win32::System::Threading::SetThreadPriority(
                        windows_sys::Win32::System::Threading::GetCurrentThread(),
                        windows_sys::Win32::System::Threading::THREAD_PRIORITY_TIME_CRITICAL,
                    );
                }
            }
        });
    }
}

// ------------------------------------------------------------------ microphone

struct Capture {
    in_ch: usize,
    step: f64, // input frames per 48 kHz frame
    pos: f64,
    prev: f32,
    pending: Vec<f32>,
    denoise: Box<nnnoiseless::DenoiseState<'static>>,
    ns_in: [f32; BLOCK],
    ns_out: [f32; BLOCK],
    frame: Vec<f32>,
    hang: u32,
    ptt_tail: u32,
    was_tx: bool,
}

impl Capture {
    fn new(in_ch: usize, rate: u32) -> Self {
        Capture {
            in_ch,
            step: rate as f64 / RATE as f64,
            pos: 0.0,
            prev: 0.0,
            pending: Vec::with_capacity(BLOCK * 8),
            denoise: nnnoiseless::DenoiseState::new(),
            ns_in: [0.0; BLOCK],
            ns_out: [0.0; BLOCK],
            frame: Vec::with_capacity(BLOCK * 4),
            hang: 0,
            ptt_tail: 0,
            was_tx: false,
        }
    }

    fn push(&mut self, samples: &[f32]) {
        mmcss();
        // Mono, then (if needed) linear resampling to 48 kHz.
        let ch = self.in_ch.max(1);
        let frames = samples.len() / ch;
        for f in 0..frames {
            let mut v = 0.0;
            for c in 0..ch {
                v += samples[f * ch + c];
            }
            v /= ch as f32;
            if self.step == 1.0 {
                self.pending.push(v);
            } else {
                // Emit every output sample that falls between prev and v.
                while self.pos <= 1.0 {
                    let t = self.pos as f32;
                    self.pending.push(self.prev + (v - self.prev) * t);
                    self.pos += self.step;
                }
                self.pos -= 1.0;
                self.prev = v;
            }
        }
        while self.pending.len() >= BLOCK {
            let block: Vec<f32> = self.pending.drain(..BLOCK).collect();
            self.process(&block);
        }
    }

    fn process(&mut self, block: &[f32]) {
        let st = SETTINGS.lock().clone().unwrap_or_default();
        let gain = st.input_gain.clamp(0.0, 4.0);
        let mut b = [0f32; BLOCK];
        for (o, i) in b.iter_mut().zip(block) {
            *o = i * gain;
        }
        if st.noise_suppression {
            for (o, i) in self.ns_in.iter_mut().zip(b.iter()) {
                *o = i * 32767.0;
            }
            self.denoise.process_frame(&mut self.ns_out, &self.ns_in);
            for (o, i) in b.iter_mut().zip(self.ns_out.iter()) {
                *o = (i / 32767.0).clamp(-1.0, 1.0);
            }
        }
        let rms = (b.iter().map(|v| v * v).sum::<f32>() / BLOCK as f32).sqrt();
        let db = if rms > 1e-9 { 20.0 * rms.log10() } else { -100.0 };
        LEVEL_DB.store(db.to_bits(), Ordering::Relaxed);
        if MIC_TEST.load(Ordering::Relaxed) {
            if let Some(lb) = LOOPBACK.lock().as_mut() {
                if lb.len() < 48000 {
                    lb.extend(b.iter().copied());
                }
            }
        }
        // The gate: voice activation with a short hang time, push-to-talk with a release tail.
        let open = match st.mode.as_str() {
            "continuous" => true,
            "ptt" => {
                if crate::ptt::held() {
                    self.ptt_tail = st.ptt_release_ms / 10;
                    true
                } else if self.ptt_tail > 0 {
                    self.ptt_tail -= 1;
                    true
                } else {
                    false
                }
            }
            _ => {
                if db > st.vad_threshold_db {
                    self.hang = 30; // 300 ms
                    true
                } else if self.hang > 0 {
                    self.hang -= 1;
                    true
                } else {
                    false
                }
            }
        };
        GATE_OPEN.store(open, Ordering::Relaxed);
        let tx = open && !SELF_MUTE.load(Ordering::Relaxed);
        TRANSMITTING.store(tx, Ordering::Relaxed);
        let per = (st.frame_ms.clamp(10, 20) / 10) as usize * BLOCK;
        let mut sink = SINK.lock();
        if tx {
            self.frame.extend_from_slice(&b);
            if self.frame.len() >= per {
                if let Some(s) = sink.as_mut() {
                    s.frame(&self.frame[..per], false);
                }
                self.frame.drain(..per);
            }
        } else if self.was_tx {
            // Finished talking: flush and tell the server it's the last packet.
            if let Some(s) = sink.as_mut() {
                let mut last = std::mem::take(&mut self.frame);
                last.resize(per, 0.0);
                s.frame(&last, true);
            }
            self.frame.clear();
        }
        self.was_tx = tx;
    }
}

fn open_input(st: &Settings, tx: mpsc::Sender<()>) -> Result<Stream, String> {
    #[cfg(windows)]
    if st.low_latency {
        let fail = Mutex::new(tx.clone());
        let make = crate::wasapi::Make::Input(Box::new(|ch, rate| {
            let mut cap = Capture::new(ch, rate);
            Box::new(move |d: &[f32]| cap.push(d))
        }));
        match crate::wasapi::open(&st.input_device, make, move || {
            let _ = fail.lock().send(());
        }) {
            Ok(s) => {
                IN_MS.store(s.latency_ms.to_bits(), Ordering::Relaxed);
                IN_LL.store(true, Ordering::Relaxed);
                util::log(format!("microphone: low-latency mode, {:.1} ms period", s.period_ms));
                return Ok(Stream::Low(s));
            }
            Err(e) => util::log(format!("microphone: low-latency mode unavailable ({e}), using standard mode")),
        }
    }
    IN_LL.store(false, Ordering::Relaxed);
    IN_MS.store(10f32.to_bits(), Ordering::Relaxed);
    open_input_cpal(st, tx).map(Stream::Cpal)
}

fn open_input_cpal(st: &Settings, tx: mpsc::Sender<()>) -> Result<cpal::Stream, String> {
    let dev = find(true, &st.input_device).ok_or("no microphone")?;
    let def = dev.default_input_config().map_err(|e| e.to_string())?;
    let fmt = def.sample_format();
    let config = def.config();
    let mut cap = Capture::new(config.channels as usize, config.sample_rate);
    let err_tx = Mutex::new(tx);
    let err = move |e: cpal::Error| {
        util::log(format!("microphone error: {e}"));
        let _ = err_tx.lock().send(());
    };
    let stream = match fmt {
        cpal::SampleFormat::F32 => dev.build_input_stream::<f32, _, _>(config, move |d: &[f32], _: &cpal::InputCallbackInfo| cap.push(d), err, None),
        cpal::SampleFormat::I16 => {
            let mut conv: Vec<f32> = Vec::with_capacity(48000);
            dev.build_input_stream::<i16, _, _>(
                config,
                move |d: &[i16], _: &cpal::InputCallbackInfo| {
                    conv.clear();
                    conv.extend(d.iter().map(|v| *v as f32 / 32768.0));
                    cap.push(&conv);
                },
                err,
                None,
            )
        }
        other => return Err(format!("unsupported microphone format {other:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok(stream)
}

// ------------------------------------------------------------------ speakers

struct Playback {
    out_ch: usize,
    step: f64, // 48 kHz frames per output frame
    pos: f64,
    src: Vec<f32>, // rendered 48 kHz stereo, not yet played
    tmp: Vec<f32>,
}

impl Playback {
    fn new(out_ch: usize, rate: u32) -> Self {
        Playback { out_ch, step: RATE as f64 / rate as f64, pos: 0.0, src: Vec::with_capacity(48000), tmp: Vec::with_capacity(48000) }
    }

    fn render48(&mut self, frames: usize) {
        let start = self.src.len();
        self.src.resize(start + frames * 2, 0.0);
        let buf = &mut self.src[start..];
        if !SELF_DEAF.load(Ordering::Relaxed) {
            if let Some(s) = SOURCE.lock().clone() {
                s.mix(buf);
            }
        }
        if let Some(lb) = LOOPBACK.lock().as_mut() {
            for f in 0..frames {
                let v = lb.pop_front().unwrap_or(0.0);
                buf[f * 2] += v;
                buf[f * 2 + 1] += v;
            }
        }
        let vol = SETTINGS.lock().as_ref().map(|s| s.output_volume).unwrap_or(1.0).clamp(0.0, 2.0);
        for v in buf.iter_mut() {
            *v = (*v * vol).clamp(-1.0, 1.0);
        }
    }

    fn fill(&mut self, out: &mut [f32]) {
        mmcss();
        let ch = self.out_ch.max(1);
        let frames = out.len() / ch;
        if self.step == 1.0 {
            self.render48(frames);
            let src = std::mem::take(&mut self.src);
            write_out(out, ch, &src[..frames * 2]);
            self.src = src;
            self.src.clear();
            return;
        }
        // Different device rate: linear interpolation from the 48 kHz render.
        let need = ((frames as f64) * self.step + self.pos).ceil() as usize + 2;
        let have = self.src.len() / 2;
        if have < need {
            self.render48(need - have);
        }
        self.tmp.clear();
        for _ in 0..frames {
            let i = self.pos.floor() as usize;
            let t = (self.pos - i as f64) as f32;
            let a = (self.src[i * 2], self.src[i * 2 + 1]);
            let b = (self.src[(i + 1) * 2], self.src[(i + 1) * 2 + 1]);
            self.tmp.push(a.0 + (b.0 - a.0) * t);
            self.tmp.push(a.1 + (b.1 - a.1) * t);
            self.pos += self.step;
        }
        let used = self.pos.floor() as usize;
        self.src.drain(..used * 2);
        self.pos -= used as f64;
        let tmp = std::mem::take(&mut self.tmp);
        write_out(out, ch, &tmp);
        self.tmp = tmp;
    }
}

fn write_out(out: &mut [f32], ch: usize, stereo: &[f32]) {
    for (f, frame) in out.chunks_exact_mut(ch).enumerate() {
        let (l, r) = (stereo.get(f * 2).copied().unwrap_or(0.0), stereo.get(f * 2 + 1).copied().unwrap_or(0.0));
        if ch == 1 {
            frame[0] = (l + r) * 0.5;
        } else {
            frame[0] = l;
            frame[1] = r;
            for x in frame.iter_mut().skip(2) {
                *x = 0.0;
            }
        }
    }
}

fn open_output(st: &Settings, tx: mpsc::Sender<()>) -> Result<Stream, String> {
    #[cfg(windows)]
    if st.low_latency {
        let fail = Mutex::new(tx.clone());
        let make = crate::wasapi::Make::Output(Box::new(|ch, rate| {
            let mut pb = Playback::new(ch, rate);
            Box::new(move |d: &mut [f32]| pb.fill(d))
        }));
        match crate::wasapi::open(&st.output_device, make, move || {
            let _ = fail.lock().send(());
        }) {
            Ok(s) => {
                OUT_MS.store(s.latency_ms.to_bits(), Ordering::Relaxed);
                OUT_LL.store(true, Ordering::Relaxed);
                util::log(format!("speakers: low-latency mode, {:.1} ms period, {:.1} ms buffer", s.period_ms, s.latency_ms));
                return Ok(Stream::Low(s));
            }
            Err(e) => util::log(format!("speakers: low-latency mode unavailable ({e}), using standard mode")),
        }
    }
    OUT_LL.store(false, Ordering::Relaxed);
    OUT_MS.store(20f32.to_bits(), Ordering::Relaxed);
    open_output_cpal(st, tx).map(Stream::Cpal)
}

fn open_output_cpal(st: &Settings, tx: mpsc::Sender<()>) -> Result<cpal::Stream, String> {
    let dev = find(false, &st.output_device).ok_or("no speakers")?;
    // Windows converts 48 kHz stereo to whatever the device runs at; if that's refused, use the
    // device's own format and convert here.
    let wanted = cpal::StreamConfig { channels: 2, sample_rate: RATE, buffer_size: cpal::BufferSize::Default };
    let attempt = |config: cpal::StreamConfig, tx: mpsc::Sender<()>| -> Result<cpal::Stream, String> {
        let mut pb = Playback::new(config.channels as usize, config.sample_rate);
        let err_tx = Mutex::new(tx);
        let err = move |e: cpal::Error| {
            util::log(format!("speaker error: {e}"));
            let _ = err_tx.lock().send(());
        };
        let s = dev
            .build_output_stream::<f32, _, _>(config, move |d: &mut [f32], _: &cpal::OutputCallbackInfo| pb.fill(d), err, None)
            .map_err(|e| e.to_string())?;
        s.play().map_err(|e| e.to_string())?;
        Ok(s)
    };
    match attempt(wanted, tx.clone()) {
        Ok(s) => Ok(s),
        Err(e) => {
            util::log(format!("48 kHz stereo refused ({e}), using the device format"));
            let def = dev.default_output_config().map_err(|e| e.to_string())?;
            attempt(def.config(), tx)
        }
    }
}
