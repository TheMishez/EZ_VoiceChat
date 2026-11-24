//! TeamSpeak 3 servers, through ReSpeak's tsclientlib (an open-source implementation of the
//! TeamSpeak 3 protocol). Voice uses the same audio core as Mumble: our microphone frames are
//! Opus-encoded here, and incoming voices go through tsclientlib's jitter buffer + decoder.
//!
//! Note: TeamSpeak's protocol is proprietary and this is an unofficial client. It targets
//! TeamSpeak 3 servers; TeamSpeak 6 servers aren't supported.
use crate::session::{self, Channel, Cmd, User, View, VoiceSink, VoiceSource};
use crate::util;
use futures::StreamExt;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tsclientlib::prelude::*;
use tsclientlib::{ChannelId, ClientId, Connection, DisconnectOptions, Identity, InMessage, MessageTarget, StreamItem};
use tsproto_packets::packets::{AudioData, CodecType, OutAudio, OutPacket};

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub nickname: String,
    pub password: String,
}

/// The identity is our "account" on TeamSpeak servers (permissions are tied to it), so it's
/// created once and kept.
fn identity() -> Identity {
    let p = util::data_dir().join("ts-identity.json");
    if let Ok(s) = std::fs::read_to_string(&p) {
        if let Ok(id) = serde_json::from_str::<Identity>(&s) {
            return id;
        }
    }
    let id = Identity::create();
    if let Ok(s) = serde_json::to_string(&id) {
        let _ = std::fs::write(&p, s);
    }
    id
}

// ------------------------------------------------------------------ voice

struct TsSink {
    enc: audiopus::coder::Encoder,
    out: Vec<u8>,
    tx: mpsc::Sender<OutPacket>,
    vid: u16,
}

impl VoiceSink for TsSink {
    fn frame(&mut self, pcm: &[f32], last: bool) {
        self.vid = self.vid.wrapping_add(1);
        if let Ok(n) = self.enc.encode_float(pcm, &mut self.out[..]) {
            let p = OutAudio::new(&AudioData::C2S { id: self.vid, codec: CodecType::OpusVoice, data: &self.out[..n] });
            let _ = self.tx.try_send(p);
        }
        if last {
            // An empty voice packet tells everyone we stopped talking.
            self.vid = self.vid.wrapping_add(1);
            let p = OutAudio::new(&AudioData::C2S { id: self.vid, codec: CodecType::OpusVoice, data: &[] });
            let _ = self.tx.try_send(p);
        }
    }
}

/// Incoming voices: tsclientlib's AudioHandler keeps a jitter-buffered queue per talker.
struct TsSource {
    handler: Mutex<tsclientlib::audio::AudioHandler<ClientId>>,
    volumes: Mutex<HashMap<u16, f32>>,
}

impl VoiceSource for TsSource {
    fn mix(&self, out: &mut [f32]) {
        let mut h = self.handler.lock();
        h.fill_buffer(out);
    }
    fn talking(&self) -> Vec<u64> {
        self.handler.lock().get_queues().keys().map(|c| c.0 as u64).collect()
    }
    fn set_volume(&self, user: u64, volume: f32) {
        let v = volume.clamp(0.0, 3.0);
        self.volumes.lock().insert(user as u16, v);
        if let Some(q) = self.handler.lock().get_mut_queues().get_mut(&ClientId(user as u16)) {
            q.volume = v;
        }
    }
}

// ------------------------------------------------------------------ the connection

pub fn connect(cfg: Config) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
    let gen = session::begin(tx);
    std::thread::Builder::new()
        .name("vc-ts3".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    session::status("error", format!("Could not start networking: {e}"));
                    session::end(gen);
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                run(&cfg, &mut rx, gen).await;
                session::end(gen);
            });
        })
        .ok();
}

/// TeamSpeak orders channels by "the channel above me": turn that chain into positions.
fn channel_positions(state: &tsclientlib::data::Connection) -> HashMap<u64, i64> {
    let mut by_parent: HashMap<u64, Vec<(u64, u64)>> = HashMap::new();
    for (id, ch) in &state.channels {
        by_parent.entry(ch.parent.0).or_default().push((id.0, ch.order.0));
    }
    let mut pos = HashMap::new();
    for (_, kids) in by_parent {
        let mut prev = 0u64;
        let mut left: Vec<(u64, u64)> = kids;
        let mut i = 0i64;
        while !left.is_empty() {
            let idx = left.iter().position(|(_, order)| *order == prev).unwrap_or(0);
            let (id, _) = left.remove(idx);
            pos.insert(id, i);
            i += 1;
            prev = id;
        }
    }
    pos
}

fn snapshot(state: &tsclientlib::data::Connection, host: &str) -> View {
    let pos = channel_positions(state);
    let mut v = View { kind: "ts3".into(), host: host.into(), server_name: state.server.name.clone(), codec: "Opus".into(), udp: true, ..Default::default() };
    for (id, ch) in &state.channels {
        v.channels.insert(
            id.0,
            Channel {
                id: id.0,
                parent: if ch.parent.0 == 0 { None } else { Some(ch.parent.0) },
                name: ch.name.clone(),
                position: pos.get(&id.0).copied().unwrap_or(0),
                description: ch.topic.clone().unwrap_or_default(),
                temporary: false,
            },
        );
    }
    for (id, c) in &state.clients {
        v.users.insert(
            id.0 as u64,
            User {
                id: id.0 as u64,
                name: c.name.clone(),
                channel: c.channel.0,
                muted: !c.input_hardware_enabled,
                deafened: !c.output_hardware_enabled,
                self_mute: c.input_muted,
                self_deaf: c.output_muted,
                registered: false,
                comment: String::new(),
            },
        );
    }
    v.me = Some(state.own_client.0 as u64);
    v
}

async fn run(cfg: &Config, cmds: &mut mpsc::UnboundedReceiver<Cmd>, gen: u64) {
    let address = format!("{}:{}", cfg.host, cfg.port);
    session::status("connecting", format!("Connecting to {address}\u{2026}"));
    let (mute, deaf) = crate::audio::self_state();
    let mut opts = Connection::build(address.clone()).name(cfg.nickname.clone()).identity(identity()).input_muted(mute).output_muted(deaf);
    if !cfg.password.is_empty() {
        opts = opts.password(cfg.password.clone());
    }
    let mut con = match opts.connect() {
        Ok(c) => c,
        Err(e) => {
            session::status("error", format!("Couldn't connect: {e}"));
            return;
        }
    };

    // Wait until we're in (the first book event = the server's state).
    let connected = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match con.events().next().await {
                Some(Ok(StreamItem::BookEvents(_))) => return Ok(()),
                Some(Ok(StreamItem::IdentityLevelIncreasing(l))) => {
                    session::status("connecting", format!("This server needs a stronger identity (level {l}), working on it\u{2026}"))
                }
                Some(Ok(_)) => {}
                Some(Err(e)) => return Err(e.to_string()),
                None => return Err("The server closed the connection".to_string()),
            }
        }
    })
    .await;
    match connected {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            session::status("error", format!("Couldn't connect: {e}"));
            return;
        }
        Err(_) => {
            session::status("error", "The server didn't answer in time");
            return;
        }
    }
    // Get every channel's clients, not just our own channel's.
    if let Ok(state) = con.get_state() {
        let _ = state.server.set_subscribed(true).send(&mut con);
    }

    let source = Arc::new(TsSource { handler: Mutex::new(tsclientlib::audio::AudioHandler::new()), volumes: Mutex::new(HashMap::new()) });
    crate::audio::set_source(Some(source.clone()));
    let (vtx, mut vrx) = mpsc::channel::<OutPacket>(64);
    let bitrate = crate::audio::settings().bitrate;
    if let Ok(mut enc) = audiopus::coder::Encoder::new(audiopus::SampleRate::Hz48000, audiopus::Channels::Mono, audiopus::Application::LowDelay) {
        let _ = enc.set_bitrate(audiopus::Bitrate::BitsPerSecond(bitrate.clamp(8000, 192000) as i32));
        crate::audio::set_sink(Some(Box::new(TsSink { enc, out: vec![0u8; 1275], tx: vtx.clone(), vid: 0 })));
    }

    let mut dirty = true;
    let mut announced = false;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            item = async { let mut ev = con.events(); ev.next().await } => {
                match item {
                    None => { session::status("disconnected", "Disconnected from the server"); break; }
                    Some(Err(e)) => { session::status("disconnected", format!("Disconnected: {e}")); break; }
                    Some(Ok(StreamItem::BookEvents(_))) => dirty = true,
                    Some(Ok(StreamItem::Audio(packet))) => {
                        let from = match packet.data().data() {
                            AudioData::S2C { from, .. } => *from,
                            AudioData::S2CWhisper { from, .. } => *from,
                            _ => continue,
                        };
                        let vol = source.volumes.lock().get(&from).copied();
                        let mut h = source.handler.lock();
                        if let Ok(Some(id)) = h.handle_packet(ClientId(from), packet) {
                            if let (Some(v), Some(q)) = (vol, h.get_mut_queues().get_mut(&id)) {
                                q.volume = v;
                            }
                        }
                    }
                    Some(Ok(StreamItem::MessageEvent(InMessage::TextMessage(m)))) => {
                        for p in m.iter() {
                            let target = format!("{:?}", p.target);
                            let kind = if target.contains("Client") { "private" } else if target.contains("Server") { "server" } else { "channel" };
                            session::chat(&p.invoker_name, &util::strip_html(&p.message), kind);
                        }
                    }
                    Some(Ok(StreamItem::DisconnectedTemporarily(r))) => {
                        session::status("reconnecting", format!("Connection interrupted ({r:?}), reconnecting\u{2026}"));
                        announced = false;
                    }
                    Some(Ok(StreamItem::IdentityLevelIncreasing(l))) => session::status("connecting", format!("Improving identity to level {l}\u{2026}")),
                    Some(Ok(_)) => {}
                }
            }
            c = cmds.recv() => {
                let Ok(state) = con.get_state() else {
                    if matches!(c, None | Some(Cmd::Disconnect)) { break; }
                    continue;
                };
                let r = match c {
                    None | Some(Cmd::Disconnect) => break,
                    Some(Cmd::Join(id)) => match state.clients.get(&state.own_client) {
                        Some(me) => me.client_move(ChannelId(id)).send(&mut con),
                        None => Ok(()),
                    },
                    Some(Cmd::Chat(text)) => state.send_message(MessageTarget::Channel, &text).send(&mut con),
                    Some(Cmd::SelfState { mute, deaf }) => state.client_update().set_input_muted(mute).set_output_muted(deaf).send(&mut con),
                };
                if let Err(e) = r {
                    session::chat("", &format!("The server refused that: {e}"), "system");
                }
            }
            p = vrx.recv() => {
                if let Some(p) = p {
                    let _ = con.send_audio(p);
                }
            }
            _ = tick.tick() => {
                if dirty && session::generation() == gen {
                    if let Ok(state) = con.get_state() {
                        dirty = false;
                        let v = snapshot(state, &address);
                        if !announced {
                            announced = true;
                            session::status("connected", format!("Connected to {}", v.server_name));
                        }
                        session::set_view(Some(v));
                    }
                }
            }
        }
    }
    crate::audio::set_sink(None);
    crate::audio::set_source(None);
    let _ = con.disconnect(DisconnectOptions::new());
    let _ = tokio::time::timeout(Duration::from_secs(2), con.events().for_each(|_| futures::future::ready(()))).await;
}
