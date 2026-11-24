//! Local test server: a small Mumble-compatible server built into the app, so you can try
//! EZ VoiceChat (or host a quick session on your network) without renting a server.
//!
//! It is deliberately minimal: fixed channels, one optional password, no accounts, ACLs, bans
//! or admin tools. The app shows a warning explaining why before it can be started; for
//! everyday use the EZ server (server/ folder) on a VPS is the recommended setup.
//!
//! Voice uses the same legacy Mumble format and OCB2 encryption as the client side, so the
//! official Mumble client can join it too.
use super::{crypt::CryptState, proto, voice};
use crate::util;
use parking_lot::Mutex;
use prost::Message;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch};

/// Bump when the warning text changes, so people see (and accept) the new version.
pub const TERMS_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// Version of the security warning the user accepted (0 = never).
    pub accepted_terms: u32,
    pub name: String,
    pub port: u16,
    /// false: this PC only (127.0.0.1). true: anyone on your network (and the internet, if you
    /// forward the port on your router).
    pub lan: bool,
    pub password: String,
    pub max_users: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { accepted_terms: 0, name: "My EZ test server".into(), port: 64738, lan: false, password: String::new(), max_users: 16 }
    }
}

fn path() -> std::path::PathBuf {
    util::data_dir().join("server.json")
}

pub fn settings() -> Settings {
    util::read_json(&path()).and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

pub fn save(mut s: Settings) {
    // Accepting the warning only happens through accept_terms().
    s.accepted_terms = settings().accepted_terms;
    s.name = s.name.trim().chars().take(60).collect();
    if s.name.is_empty() {
        s.name = Settings::default().name;
    }
    s.max_users = s.max_users.clamp(2, 64);
    if let Ok(v) = serde_json::to_value(&s) {
        util::write_json(&path(), &v);
    }
}

pub fn accept_terms() {
    let mut s = settings();
    s.accepted_terms = TERMS_VERSION;
    if let Ok(v) = serde_json::to_value(&s) {
        util::write_json(&path(), &v);
    }
}

// ------------------------------------------------------------------ server state

const ROOT: u32 = 0;
const LOBBY: u32 = 1;
const CHANNELS: &[(u32, &str)] = &[(LOBBY, "Lobby"), (2, "Team 1"), (3, "Team 2"), (4, "AFK")];
/// Traverse, Enter, Speak, Whisper, TextMessage.
const PERMS: u32 = 0x2 | 0x4 | 0x8 | 0x100 | 0x200;
const BANDWIDTH: u32 = 144_000;
const MAX_FRAME: usize = 64 * 1024;

struct Client {
    name: String,
    channel: u32,
    self_mute: bool,
    self_deaf: bool,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    crypt: CryptState,
    ip: IpAddr,
    udp: Option<SocketAddr>,
    last_udp: Option<Instant>,
}

struct State {
    name: String,
    password: String,
    max_users: u32,
    clients: HashMap<u32, Client>,
    next: u32,
    udp: Option<Arc<UdpSocket>>,
    /// Wrong-password attempts per address: (count, first attempt).
    fails: HashMap<IpAddr, (u32, Instant)>,
}

struct Running {
    stop: watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
    state: Arc<Mutex<State>>,
    port: u16,
    lan: bool,
}

static RUN: Mutex<Option<Running>> = parking_lot::const_mutex(None);

fn lan_ip() -> Option<String> {
    // Connecting a UDP socket sends nothing; it just tells us which local address would be used.
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

pub fn status() -> Value {
    let r = RUN.lock();
    match &*r {
        Some(x) => {
            let users: Vec<String> = x.state.lock().clients.values().map(|c| c.name.clone()).collect();
            json!({ "running": true, "port": x.port, "lan": x.lan, "lanIp": if x.lan { lan_ip() } else { None }, "users": users })
        }
        None => json!({ "running": false }),
    }
}

fn emit_users(st: &State) {
    let users: Vec<&str> = st.clients.values().map(|c| c.name.as_str()).collect();
    util::emit("srv-users", json!({ "users": users }));
}

pub fn stop() {
    let r = RUN.lock().take();
    if let Some(mut r) = r {
        let _ = r.stop.send(true);
        if let Some(t) = r.thread.take() {
            let _ = t.join();
        }
        util::log("local server stopped");
    }
    util::emit("srv-status", status());
}

pub fn start() -> Result<Value, String> {
    let s = settings();
    if s.accepted_terms < TERMS_VERSION {
        return Err("Read and accept the local server warning first.".into());
    }
    if s.port < 1024 {
        return Err("Pick a port of 1024 or higher.".into());
    }
    stop();
    let bind: IpAddr = if s.lan { [0, 0, 0, 0].into() } else { [127, 0, 0, 1].into() };
    let addr = SocketAddr::new(bind, s.port);
    let in_use = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::AddrInUse {
            format!("Port {} is already in use (is another Mumble server running?). Pick another port.", s.port)
        } else {
            format!("Could not open port {}: {e}", s.port)
        }
    };
    let tcp = std::net::TcpListener::bind(addr).map_err(in_use)?;
    let udp = std::net::UdpSocket::bind(addr).map_err(in_use)?;
    tcp.set_nonblocking(true).map_err(|e| e.to_string())?;
    udp.set_nonblocking(true).map_err(|e| e.to_string())?;
    let acceptor = acceptor()?;
    let state = Arc::new(Mutex::new(State {
        name: s.name.clone(),
        password: s.password.clone(),
        max_users: s.max_users,
        clients: HashMap::new(),
        next: 0,
        udp: None,
        fails: HashMap::new(),
    }));
    let (stop_tx, stop_rx) = watch::channel(false);
    let st = state.clone();
    let thread = std::thread::Builder::new()
        .name("vc-server".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => return util::log(format!("local server: {e}")),
            };
            rt.block_on(serve(tcp, udp, acceptor, st, stop_rx));
        })
        .map_err(|e| e.to_string())?;
    *RUN.lock() = Some(Running { stop: stop_tx, thread: Some(thread), state, port: s.port, lan: s.lan });
    util::log(format!("local server started on {addr}"));
    let v = status();
    util::emit("srv-status", v.clone());
    Ok(v)
}

/// The server's own certificate, kept so clients that trusted it once don't get warned again.
fn acceptor() -> Result<tokio_rustls::TlsAcceptor, String> {
    let dir = util::data_dir();
    let (cp, kp) = (dir.join("server-cert.der"), dir.join("server-key.der"));
    let (c, k) = match (std::fs::read(&cp), std::fs::read(&kp)) {
        (Ok(c), Ok(k)) => (c, k),
        _ => {
            let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "EZ VoiceChat local server".to_string()]).map_err(|e| e.to_string())?;
            let (c, k) = (ck.cert.der().to_vec(), ck.key_pair.serialize_der());
            let _ = std::fs::write(&cp, &c);
            let _ = std::fs::write(&kp, &k);
            (c, k)
        }
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(c)],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(k)),
        )
        .map_err(|e| e.to_string())?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(cfg)))
}

async fn serve(tcp: std::net::TcpListener, udp: std::net::UdpSocket, acceptor: tokio_rustls::TlsAcceptor, state: Arc<Mutex<State>>, mut stop: watch::Receiver<bool>) {
    let (tcp, udp) = match (TcpListener::from_std(tcp), UdpSocket::from_std(udp)) {
        (Ok(t), Ok(u)) => (t, Arc::new(u)),
        _ => return util::log("local server: could not use the sockets"),
    };
    state.lock().udp = Some(udp.clone());
    let udp_task = tokio::spawn(udp_loop(udp, state.clone()));
    loop {
        tokio::select! {
            r = tcp.accept() => {
                if let Ok((s, a)) = r {
                    tokio::spawn(handle(s, a, acceptor.clone(), state.clone()));
                }
            }
            _ = stop.changed() => break,
        }
    }
    udp_task.abort();
    state.lock().clients.clear(); // closes every connection
}

async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<(u16, Vec<u8>)> {
    let mut h = [0u8; 6];
    r.read_exact(&mut h).await?;
    let kind = u16::from_be_bytes([h[0], h[1]]);
    let len = u32::from_be_bytes([h[2], h[3], h[4], h[5]]) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok((kind, body))
}

async fn handle(stream: TcpStream, addr: SocketAddr, acceptor: tokio_rustls::TlsAcceptor, state: Arc<Mutex<State>>) {
    let _ = stream.set_nodelay(true);
    let tls = match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await {
        Ok(Ok(t)) => t,
        _ => return,
    };
    let (mut rd, mut wr) = tokio::io::split(tls);
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(b) = rx.recv().await {
            if wr.write_all(&b).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    });
    let version = proto::Version {
        version_v1: Some(0x0001_0400),
        release: Some("EZ VoiceChat local server".into()),
        os: Some("Windows".into()),
        os_version: None,
        version_v2: Some((1u64 << 48) | (4u64 << 32)),
    };
    let _ = tx.send(proto::frame(proto::VERSION, &version));
    let mut session: Option<u32> = None;
    loop {
        // Clients ping every 15 s; 35 s of silence means they're gone.
        let Ok(Ok((kind, body))) = tokio::time::timeout(Duration::from_secs(35), read_frame(&mut rd)).await else { break };
        match session {
            None => match kind {
                proto::AUTHENTICATE => match authenticate(&state, &body, addr.ip(), &tx) {
                    Some(s) => session = Some(s),
                    None => break,
                },
                proto::VERSION | proto::PING => {}
                _ => break,
            },
            Some(s) => {
                if !on_message(&state, s, kind, &body) {
                    break;
                }
            }
        }
    }
    if let Some(s) = session {
        remove(&state, s);
    }
    drop(tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
}

fn user_state(id: u32, c: &Client) -> proto::UserState {
    proto::UserState {
        session: Some(id),
        name: Some(c.name.clone()),
        channel_id: Some(c.channel),
        self_mute: Some(c.self_mute),
        self_deaf: Some(c.self_deaf),
        ..Default::default()
    }
}

fn broadcast(st: &State, frame: &[u8]) {
    for c in st.clients.values() {
        let _ = c.tx.send(frame.to_vec());
    }
}

fn random16() -> [u8; 16] {
    let mut b = [0u8; 16];
    let _ = SystemRandom::new().fill(&mut b);
    b
}

fn authenticate(state: &Mutex<State>, body: &[u8], ip: IpAddr, tx: &mpsc::UnboundedSender<Vec<u8>>) -> Option<u32> {
    let a = proto::Authenticate::decode(body).ok()?;
    let reject = |t: i32, why: &str| {
        let _ = tx.send(proto::frame(proto::REJECT, &proto::Reject { r#type: Some(t), reason: Some(why.into()) }));
        None
    };
    let mut st = state.lock();
    // Slow down password guessing: 5 wrong tries locks that address out for 5 minutes.
    if let Some((n, since)) = st.fails.get(&ip) {
        if *n >= 5 && since.elapsed() < Duration::from_secs(300) {
            return reject(4, "Too many wrong passwords. Try again in a few minutes.");
        }
    }
    if !st.password.is_empty() && a.password.as_deref().unwrap_or("") != st.password {
        let e = st.fails.entry(ip).or_insert((0, Instant::now()));
        if e.1.elapsed() > Duration::from_secs(300) {
            *e = (0, Instant::now());
        }
        e.0 += 1;
        return reject(4, "Wrong server password");
    }
    let name: String = a.username.unwrap_or_default().trim().chars().filter(|c| !c.is_control()).take(64).collect();
    if name.is_empty() {
        return reject(2, "Pick a nickname");
    }
    if st.clients.values().any(|c| c.name.eq_ignore_ascii_case(&name)) {
        return reject(5, "That nickname is already in use on this server");
    }
    if st.clients.len() as u32 >= st.max_users {
        return reject(6, "The server is full");
    }
    st.fails.remove(&ip);
    st.next += 1;
    let s = st.next;

    let (key, cn, sn) = (random16(), random16(), random16());
    let mut crypt = CryptState::default();
    crypt.set_key(&key, &sn, &cn); // the server encrypts with the server nonce
    let _ = tx.send(proto::frame(
        proto::CRYPT_SETUP,
        &proto::CryptSetup { key: Some(key.to_vec()), client_nonce: Some(cn.to_vec()), server_nonce: Some(sn.to_vec()) },
    ));
    let _ = tx.send(proto::frame(proto::CODEC_VERSION, &proto::CodecVersion { alpha: -2147483637, beta: 0, prefer_alpha: true, opus: Some(true) }));
    let root = proto::ChannelState { channel_id: Some(ROOT), name: Some(st.name.clone()), position: Some(0), ..Default::default() };
    let _ = tx.send(proto::frame(proto::CHANNEL_STATE, &root));
    for (i, (id, n)) in CHANNELS.iter().enumerate() {
        let c = proto::ChannelState { channel_id: Some(*id), parent: Some(ROOT), name: Some((*n).into()), position: Some(i as i32), ..Default::default() };
        let _ = tx.send(proto::frame(proto::CHANNEL_STATE, &c));
    }
    for (id, c) in &st.clients {
        let _ = tx.send(proto::frame(proto::USER_STATE, &user_state(*id, c)));
    }
    let client = Client { name, channel: LOBBY, self_mute: false, self_deaf: false, tx: tx.clone(), crypt, ip, udp: None, last_udp: None };
    let us = user_state(s, &client);
    st.clients.insert(s, client);
    broadcast(&st, &proto::frame(proto::USER_STATE, &us));
    let welcome = format!("Welcome to {}. This is a local test server running inside EZ VoiceChat.", util::escape_html(&st.name));
    let _ = tx.send(proto::frame(
        proto::SERVER_SYNC,
        &proto::ServerSync { session: Some(s), max_bandwidth: Some(BANDWIDTH), welcome_text: Some(welcome.clone()), permissions: Some(PERMS as u64) },
    ));
    let _ = tx.send(proto::frame(
        proto::SERVER_CONFIG,
        &proto::ServerConfig {
            max_bandwidth: Some(BANDWIDTH),
            welcome_text: Some(welcome),
            allow_html: Some(false),
            message_length: Some(5000),
            image_message_length: Some(0),
            max_users: Some(st.max_users),
        },
    ));
    emit_users(&st);
    Some(s)
}

fn remove(state: &Mutex<State>, s: u32) {
    let mut st = state.lock();
    if st.clients.remove(&s).is_some() {
        broadcast(&st, &proto::frame(proto::USER_REMOVE, &proto::UserRemove { session: s, actor: None, reason: None, ban: None }));
        emit_users(&st);
    }
}

fn on_message(state: &Mutex<State>, s: u32, kind: u16, body: &[u8]) -> bool {
    match kind {
        proto::PING => {
            let p = proto::Ping::decode(body).unwrap_or_default();
            let st = state.lock();
            if let Some(c) = st.clients.get(&s) {
                let reply = proto::Ping {
                    timestamp: p.timestamp,
                    good: Some(c.crypt.good),
                    late: Some(c.crypt.late),
                    lost: Some(c.crypt.lost),
                    resync: Some(c.crypt.resync),
                    ..Default::default()
                };
                let _ = c.tx.send(proto::frame(proto::PING, &reply));
            }
        }
        proto::UDP_TUNNEL => {
            if body.len() <= 4096 {
                relay(&mut state.lock(), s, body);
            }
        }
        proto::USER_STATE => {
            let Ok(m) = proto::UserState::decode(body) else { return true };
            if m.session.is_some_and(|x| x != s) {
                return true; // no moving or muting other people
            }
            let mut st = state.lock();
            let Some(c) = st.clients.get_mut(&s) else { return false };
            let mut out = proto::UserState { session: Some(s), actor: Some(s), ..Default::default() };
            if let Some(ch) = m.channel_id {
                if ch == ROOT || CHANNELS.iter().any(|(id, _)| *id == ch) {
                    c.channel = ch;
                    out.channel_id = Some(ch);
                }
            }
            if let Some(v) = m.self_mute {
                c.self_mute = v;
                out.self_mute = Some(v);
            }
            if let Some(v) = m.self_deaf {
                c.self_deaf = v;
                out.self_deaf = Some(v);
                if v {
                    c.self_mute = true;
                    out.self_mute = Some(true);
                }
            }
            if let Some(cm) = m.comment {
                out.comment = Some(cm.chars().take(500).collect());
            }
            broadcast(&st, &proto::frame(proto::USER_STATE, &out));
        }
        proto::TEXT_MESSAGE => {
            let Ok(m) = proto::TextMessage::decode(body) else { return true };
            let text: String = m.message.chars().take(5000).collect();
            if text.trim().is_empty() {
                return true;
            }
            let st = state.lock();
            let out = proto::TextMessage { actor: Some(s), session: m.session.clone(), channel_id: m.channel_id.clone(), tree_id: vec![], message: text };
            let f = proto::frame(proto::TEXT_MESSAGE, &out);
            let everyone = !m.tree_id.is_empty();
            for (id, c) in &st.clients {
                if *id != s && (everyone || m.session.contains(id) || m.channel_id.contains(&c.channel)) {
                    let _ = c.tx.send(f.clone());
                }
            }
        }
        proto::CRYPT_SETUP => {
            let m = proto::CryptSetup::decode(body).unwrap_or_default();
            let mut st = state.lock();
            let Some(c) = st.clients.get_mut(&s) else { return false };
            match m.client_nonce {
                Some(n) if n.len() == 16 => c.crypt.set_decrypt_iv(&n),
                _ => {
                    let reply = proto::CryptSetup { key: None, client_nonce: None, server_nonce: Some(c.crypt.encrypt_iv.to_vec()) };
                    let _ = c.tx.send(proto::frame(proto::CRYPT_SETUP, &reply));
                }
            }
        }
        proto::PERMISSION_QUERY => {
            let m = proto::PermissionQuery::decode(body).unwrap_or_default();
            let st = state.lock();
            if let Some(c) = st.clients.get(&s) {
                let reply = proto::PermissionQuery { channel_id: m.channel_id, permissions: Some(PERMS), flush: None };
                let _ = c.tx.send(proto::frame(proto::PERMISSION_QUERY, &reply));
            }
        }
        _ => {}
    }
    true
}

/// Forwards one voice packet (client format) to everyone else in the sender's channel, over
/// UDP when that listener's UDP works and through their TCP connection otherwise.
fn relay(st: &mut State, from: u32, pkt: &[u8]) {
    let Some(&h) = pkt.first() else { return };
    if h >> 5 != voice::TYPE_OPUS {
        return;
    }
    let target = h & 0x1F;
    let Some(src) = st.clients.get(&from) else { return };
    if src.self_mute {
        return;
    }
    let chan = src.channel;
    let mut out = Vec::with_capacity(pkt.len() + 4);
    out.push(voice::TYPE_OPUS << 5);
    voice::put_varint(&mut out, from as u64);
    out.extend_from_slice(&pkt[1..]);
    let ids: Vec<u32> = match target {
        31 => vec![from], // "server loopback" (mic test in the official client)
        0 => st.clients.iter().filter(|(id, c)| **id != from && c.channel == chan && !c.self_deaf).map(|(id, _)| *id).collect(),
        _ => return, // whispers aren't supported here
    };
    let tunnel = proto::raw_frame(proto::UDP_TUNNEL, &out);
    let udp = st.udp.clone();
    for id in ids {
        let Some(c) = st.clients.get_mut(&id) else { continue };
        match (c.udp, c.last_udp, &udp) {
            (Some(a), Some(t), Some(sock)) if t.elapsed() < Duration::from_secs(20) => {
                let enc = c.crypt.encrypt(&out);
                let _ = sock.try_send_to(&enc, a);
            }
            _ => {
                let _ = c.tx.send(tunnel.clone());
            }
        }
    }
}

async fn udp_loop(sock: Arc<UdpSocket>, state: Arc<Mutex<State>>) {
    let mut buf = [0u8; 2048];
    loop {
        let Ok((n, from)) = sock.recv_from(&mut buf).await else { continue };
        let d = &buf[..n];
        // Mumble's server-list ping (unencrypted): version, echoed id, users, max users, bandwidth.
        if n == 12 && d[..4] == [0, 0, 0, 0] {
            let st = state.lock();
            let mut r = Vec::with_capacity(24);
            r.extend_from_slice(&[0, 1, 4, 0]);
            r.extend_from_slice(&d[4..12]);
            r.extend_from_slice(&(st.clients.len() as u32).to_be_bytes());
            r.extend_from_slice(&st.max_users.to_be_bytes());
            r.extend_from_slice(&BANDWIDTH.to_be_bytes());
            let _ = sock.try_send_to(&r, from);
            continue;
        }
        let mut st = state.lock();
        let mut found: Option<(u32, Vec<u8>)> = None;
        if let Some((id, c)) = st.clients.iter_mut().find(|(_, c)| c.udp == Some(from)) {
            if let Some(p) = c.crypt.decrypt(d) {
                found = Some((*id, p));
            }
        } else {
            // A new UDP address: find the client it belongs to by trying their keys.
            for (id, c) in st.clients.iter_mut() {
                if c.ip != from.ip() {
                    continue;
                }
                let mut trial = c.crypt.clone();
                if let Some(p) = trial.decrypt(d) {
                    c.crypt = trial;
                    c.udp = Some(from);
                    found = Some((*id, p));
                    break;
                }
            }
        }
        let Some((id, p)) = found else { continue };
        let is_ping = p.first().map(|h| h >> 5) == Some(voice::TYPE_PING);
        if let Some(c) = st.clients.get_mut(&id) {
            c.last_udp = Some(Instant::now());
            if is_ping {
                let enc = c.crypt.encrypt(&p);
                let _ = sock.try_send_to(&enc, from);
                continue;
            }
        }
        relay(&mut st, id, &p);
    }
}
