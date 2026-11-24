//! Mumble voice: the (legacy) UDP packet format, Opus encode/decode, and a per-talker jitter
//! buffer that feeds the shared audio core.
//!
//! Packet (client -> server):  [type 4 << 5 | target] [varint sequence] [varint opus len | 0x2000 if last] [opus]
//! Packet (server -> client):  [type 4 << 5 | target] [varint session] [varint sequence] [varint len] [opus]
//! Sequence numbers count 10 ms frames.
use crate::session::{VoiceSink, VoiceSource};
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub const TYPE_PING: u8 = 1;
pub const TYPE_OPUS: u8 = 4;
const FRAME: usize = 480; // 10 ms at 48 kHz

// ------------------------------------------------------------------ Mumble's varint

pub fn put_varint(out: &mut Vec<u8>, v: u64) {
    if v < 0x80 {
        out.push(v as u8);
    } else if v < 0x4000 {
        out.push(((v >> 8) | 0x80) as u8);
        out.push(v as u8);
    } else if v < 0x20_0000 {
        out.push(((v >> 16) | 0xC0) as u8);
        out.push((v >> 8) as u8);
        out.push(v as u8);
    } else if v < 0x1000_0000 {
        out.push(((v >> 24) | 0xE0) as u8);
        out.push((v >> 16) as u8);
        out.push((v >> 8) as u8);
        out.push(v as u8);
    } else if v < 0x1_0000_0000 {
        out.push(0xF0);
        out.extend_from_slice(&(v as u32).to_be_bytes());
    } else {
        out.push(0xF4);
        out.extend_from_slice(&v.to_be_bytes());
    }
}

pub struct Reader<'a> {
    d: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Reader { d, pos: 0 }
    }
    fn byte(&mut self) -> Option<u64> {
        let b = *self.d.get(self.pos)?;
        self.pos += 1;
        Some(b as u64)
    }
    pub fn varint(&mut self) -> Option<u64> {
        let v = self.byte()?;
        Some(if v & 0x80 == 0 {
            v & 0x7F
        } else if v & 0xC0 == 0x80 {
            ((v & 0x3F) << 8) | self.byte()?
        } else if v & 0xF0 == 0xF0 {
            match v & 0xFC {
                0xF0 => (self.byte()? << 24) | (self.byte()? << 16) | (self.byte()? << 8) | self.byte()?,
                0xF4 => {
                    let mut x = 0u64;
                    for _ in 0..8 {
                        x = (x << 8) | self.byte()?;
                    }
                    x
                }
                0xF8 => !self.varint()?,
                _ => !(v & 0x03),
            }
        } else if v & 0xF0 == 0xE0 {
            ((v & 0x0F) << 24) | (self.byte()? << 16) | (self.byte()? << 8) | self.byte()?
        } else if v & 0xE0 == 0xC0 {
            ((v & 0x1F) << 16) | (self.byte()? << 8) | self.byte()?
        } else {
            return None;
        })
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }
}

pub fn ping_packet(ts: u64) -> Vec<u8> {
    let mut v = vec![TYPE_PING << 5];
    put_varint(&mut v, ts);
    v
}

pub fn audio_packet(seq: u64, opus: &[u8], last: bool) -> Vec<u8> {
    let mut v = Vec::with_capacity(opus.len() + 8);
    v.push(TYPE_OPUS << 5); // target 0 = normal talking
    put_varint(&mut v, seq);
    put_varint(&mut v, opus.len() as u64 | if last { 0x2000 } else { 0 });
    v.extend_from_slice(opus);
    v
}

pub enum Incoming {
    Ping(u64),
    Audio { session: u32, seq: u64, opus: Vec<u8>, last: bool },
    Other,
}

pub fn parse_incoming(d: &[u8]) -> Incoming {
    let Some(&h) = d.first() else { return Incoming::Other };
    let kind = (h >> 5) & 7;
    let mut r = Reader::new(&d[1..]);
    match kind {
        TYPE_PING => r.varint().map(Incoming::Ping).unwrap_or(Incoming::Other),
        TYPE_OPUS => {
            let (Some(session), Some(seq), Some(hdr)) = (r.varint(), r.varint(), r.varint()) else { return Incoming::Other };
            let len = (hdr & 0x1FFF) as usize;
            let last = hdr & 0x2000 != 0;
            match r.take(len) {
                Some(opus) => Incoming::Audio { session: session as u32, seq, opus: opus.to_vec(), last },
                None => Incoming::Other,
            }
        }
        _ => Incoming::Other, // CELT / Speex: not supported
    }
}

// ------------------------------------------------------------------ microphone -> server

pub struct Sink {
    enc: audiopus::coder::Encoder,
    seq: u64,
    out: Vec<u8>,
    tx: mpsc::Sender<Vec<u8>>,
    talking: bool,
}

impl Sink {
    pub fn new(tx: mpsc::Sender<Vec<u8>>, bitrate: u32) -> Option<Self> {
        let mut enc = audiopus::coder::Encoder::new(audiopus::SampleRate::Hz48000, audiopus::Channels::Mono, audiopus::Application::Voip).ok()?;
        let _ = enc.set_bitrate(audiopus::Bitrate::BitsPerSecond(bitrate.clamp(8000, 192000) as i32));
        Some(Sink { enc, seq: 0, out: vec![0u8; 1275], tx, talking: false })
    }
}

impl VoiceSink for Sink {
    fn frame(&mut self, pcm: &[f32], last: bool) {
        if !self.talking {
            // New talk spurt: jump ahead so the server's jitter buffer restarts cleanly.
            self.seq += 100;
            self.talking = true;
        }
        if let Ok(n) = self.enc.encode_float(pcm, &mut self.out[..]) {
            let _ = self.tx.try_send(audio_packet(self.seq, &self.out[..n], last));
        }
        self.seq += (pcm.len() / FRAME).max(1) as u64;
        if last {
            self.talking = false;
        }
    }
}

// ------------------------------------------------------------------ server -> speakers

struct Talker {
    dec: audiopus::coder::Decoder,
    fifo: VecDeque<f32>,
    next_seq: Option<u64>,
    buffering: bool,
    ended: bool,
    last_packet: Instant,
    volume: f32,
    tmp: Vec<f32>,
}

/// Jitter buffer: starts playing a talker after ~30 ms, re-buffers after an underrun, conceals
/// small packet losses with Opus PLC, and drops audio that's more than 200 ms behind.
pub struct Mixer {
    talkers: Mutex<HashMap<u32, Talker>>,
    volumes: Mutex<HashMap<u32, f32>>,
}

const START_SAMPLES: usize = FRAME * 3;
const MAX_SAMPLES: usize = FRAME * 20;

impl Mixer {
    pub fn new() -> Self {
        Mixer { talkers: Mutex::new(HashMap::new()), volumes: Mutex::new(HashMap::new()) }
    }

    pub fn push(&self, session: u32, seq: u64, opus: &[u8], last: bool) {
        let vol = self.volumes.lock().get(&session).copied().unwrap_or(1.0);
        let mut map = self.talkers.lock();
        let t = match map.get_mut(&session) {
            Some(t) => t,
            None => {
                let Ok(dec) = audiopus::coder::Decoder::new(audiopus::SampleRate::Hz48000, audiopus::Channels::Mono) else { return };
                map.insert(
                    session,
                    Talker {
                        dec,
                        fifo: VecDeque::with_capacity(MAX_SAMPLES),
                        next_seq: None,
                        buffering: true,
                        ended: false,
                        last_packet: Instant::now(),
                        volume: vol,
                        tmp: vec![0.0; FRAME * 12],
                    },
                );
                map.get_mut(&session).unwrap()
            }
        };
        t.volume = vol;
        // A new talk spurt (or a big jump): start over.
        if t.ended || t.next_seq.map(|n| seq > n + 50 || seq + 50 < n).unwrap_or(true) {
            t.next_seq = Some(seq);
            t.buffering = true;
            t.ended = false;
        }
        let expected = t.next_seq.unwrap_or(seq);
        if seq < expected {
            return; // late or duplicate
        }
        // Lost packets: let Opus conceal up to 60 ms.
        let gap = (seq - expected).min(6) as usize;
        if gap > 0 {
            let n = gap * FRAME;
            if let Ok(got) = t.dec.decode_float(None, (&mut t.tmp[..n]).try_into().unwrap(), false) {
                t.fifo.extend(t.tmp[..got].iter().copied());
            }
        }
        if !opus.is_empty() {
            let pkt = opus.try_into();
            let out = (&mut t.tmp[..]).try_into();
            if let (Ok(pkt), Ok(out)) = (pkt, out) {
                if let Ok(got) = t.dec.decode_float(Some(pkt), out, false) {
                    t.fifo.extend(t.tmp[..got].iter().copied());
                    t.next_seq = Some(seq + (got / FRAME).max(1) as u64);
                }
            }
        }
        while t.fifo.len() > MAX_SAMPLES {
            t.fifo.pop_front();
        }
        t.last_packet = Instant::now();
        if last {
            t.ended = true;
        }
    }

    pub fn remove(&self, session: u32) {
        self.talkers.lock().remove(&session);
    }

    pub fn clear(&self) {
        self.talkers.lock().clear();
    }
}

impl VoiceSource for Mixer {
    fn mix(&self, out: &mut [f32]) {
        let frames = out.len() / 2;
        let mut map = self.talkers.lock();
        map.retain(|_, t| !(t.fifo.is_empty() && t.last_packet.elapsed() > Duration::from_secs(5)));
        for t in map.values_mut() {
            if t.buffering {
                if t.fifo.len() < START_SAMPLES && !t.ended {
                    continue;
                }
                t.buffering = false;
            }
            for f in 0..frames {
                match t.fifo.pop_front() {
                    Some(v) => {
                        let v = v * t.volume;
                        out[f * 2] += v;
                        out[f * 2 + 1] += v;
                    }
                    None => {
                        if !t.ended {
                            t.buffering = true; // underrun: wait for a little more audio
                        }
                        break;
                    }
                }
            }
        }
    }

    fn talking(&self) -> Vec<u64> {
        self.talkers
            .lock()
            .iter()
            .filter(|(_, t)| !t.fifo.is_empty() || (!t.ended && t.last_packet.elapsed() < Duration::from_millis(120)))
            .map(|(id, _)| *id as u64)
            .collect()
    }

    fn set_volume(&self, user: u64, volume: f32) {
        self.volumes.lock().insert(user as u32, volume.clamp(0.0, 3.0));
        if let Some(t) = self.talkers.lock().get_mut(&(user as u32)) {
            t.volume = volume.clamp(0.0, 3.0);
        }
    }
}
