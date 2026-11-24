//! A Mumble client: TLS control channel (protobuf messages) + encrypted UDP voice, falling
//! back to tunnelling voice through TCP when UDP is blocked. Works with any Mumble 1.2+ server
//! (we speak the classic voice packet format, which every server supports).
pub mod crypt;
pub mod proto;
pub mod voice;

use crate::session::{self, Channel, Cmd, User, View};
use crate::util;
use prost::Message;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// A certificate fingerprint the user agreed to trust (after a "certificate changed" warning).
    pub trust: Option<String>,
}

enum End {
    /// The user disconnected (or we were kicked / rejected): don't reconnect.
    Stop,
    /// The connection dropped: try again.
    Retry(String),
}

pub fn connect(cfg: Config) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Cmd>();
    let gen = session::begin(tx);
    std::thread::Builder::new()
        .name("vc-mumble".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    session::status("error", format!("Could not start networking: {e}"));
                    session::end(gen);
                    return;
                }
            };
            rt.block_on(async move {
                let mut attempt = 0u32;
                loop {
                    match run(&cfg, &mut rx, gen).await {
                        End::Stop => break,
                        End::Retry(why) => {
                            if session::generation() != gen {
                                break;
                            }
                            attempt += 1;
                            let wait = Duration::from_secs((2u64 << attempt.min(4)).min(30));
                            session::status("reconnecting", format!("{why}. Reconnecting in {}s\u{2026}", wait.as_secs()));
                            // Wait, but stop right away if the user disconnects meanwhile.
                            let deadline = tokio::time::sleep(wait);
                            tokio::pin!(deadline);
                            loop {
                                tokio::select! {
                                    _ = &mut deadline => break,
                                    c = rx.recv() => match c {
                                        Some(Cmd::Disconnect) | None => { session::end(gen); return; }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }
                session::end(gen);
            });
        })
        .ok();
}

// ------------------------------------------------------------------ TLS

#[derive(Debug)]
struct AcceptAny(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // Mumble servers almost always use self-signed certificates. We pin the fingerprint
        // instead (trust on first use), like the official client.
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Our own certificate: lets the server recognise us (needed to register a name on it).
fn client_identity() -> Option<(Vec<u8>, Vec<u8>)> {
    let dir = util::data_dir();
    let (cp, kp) = (dir.join("mumble-cert.der"), dir.join("mumble-key.der"));
    if let (Ok(c), Ok(k)) = (std::fs::read(&cp), std::fs::read(&kp)) {
        return Some((c, k));
    }
    let ck = rcgen::generate_simple_self_signed(vec!["EZ VoiceChat".to_string()]).ok()?;
    let c = ck.cert.der().to_vec();
    let k = ck.key_pair.serialize_der();
    let _ = std::fs::write(&cp, &c);
    let _ = std::fs::write(&kp, &k);
    Some((c, k))
}

fn tls_config() -> Result<rustls::ClientConfig, String> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let b = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)));
    let cfg = match client_identity() {
        Some((c, k)) => b
            .with_client_auth_cert(
                vec![rustls::pki_types::CertificateDer::from(c)],
                rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(k)),
            )
            .map_err(|e| e.to_string())?,
        None => b.with_no_client_auth(),
    };
    Ok(cfg)
}

fn known_path() -> std::path::PathBuf {
    util::data_dir().join("known-servers.json")
}

fn sha1_hex(d: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, d).as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

// ------------------------------------------------------------------ the connection

async fn read_frame<R: AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<(u16, Vec<u8>)> {
    let mut h = [0u8; 6];
    r.read_exact(&mut h).await?;
    let kind = u16::from_be_bytes([h[0], h[1]]);
    let len = u32::from_be_bytes([h[2], h[3], h[4], h[5]]) as usize;
    if len > 8 << 20 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "message too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok((kind, body))
}

fn merge_channel(v: &mut View, m: proto::ChannelState) {
    let Some(id) = m.channel_id else { return };
    let c = v.channels.entry(id as u64).or_insert_with(|| Channel { id: id as u64, ..Default::default() });
    if let Some(p) = m.parent {
        c.parent = if p == id { None } else { Some(p as u64) };
    }
    if let Some(n) = m.name {
        c.name = n;
    }
    if let Some(d) = m.description {
        c.description = util::strip_html(&d);
    }
    if let Some(p) = m.position {
        c.position = p as i64;
    }
    if let Some(t) = m.temporary {
        c.temporary = t;
    }
}

fn merge_user(v: &mut View, m: &proto::UserState) {
    let Some(s) = m.session else { return };
    let u = v.users.entry(s as u64).or_insert_with(|| User { id: s as u64, ..Default::default() });
    if let Some(n) = &m.name {
        u.name = n.clone();
    }
    if let Some(c) = m.channel_id {
        u.channel = c as u64;
    }
    if let Some(x) = m.mute {
        u.muted = x;
    }
    if let Some(x) = m.suppress {
        u.muted = x || u.muted;
    }
    if let Some(x) = m.deaf {
        u.deafened = x;
    }
    if let Some(x) = m.self_mute {
        u.self_mute = x;
    }
    if let Some(x) = m.self_deaf {
        u.self_deaf = x;
    }
    if let Some(x) = m.user_id {
        u.registered = x > 0 || u.registered;
    }
    if let Some(c) = &m.comment {
        u.comment = util::strip_html(c);
    }
}

async fn run(cfg: &Config, cmds: &mut mpsc::UnboundedReceiver<Cmd>, gen: u64) -> End {
    session::status("connecting", format!("Connecting to {}:{}\u{2026}", cfg.host, cfg.port));
    let addr: SocketAddr = match tokio::net::lookup_host((cfg.host.as_str(), cfg.port)).await.ok().and_then(|mut a| a.next()) {
        Some(a) => a,
        None => return End::Retry(format!("Couldn't find {}", cfg.host)),
    };
    let tcp = match tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect(addr)).await {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return End::Retry(format!("Couldn't reach the server ({e})")),
        Err(_) => return End::Retry("The server didn't answer".into()),
    };
    let _ = tcp.set_nodelay(true);
    let tls_cfg = match tls_config() {
        Ok(c) => c,
        Err(e) => {
            session::status("error", format!("TLS setup failed: {e}"));
            return End::Stop;
        }
    };
    let name = match rustls::pki_types::ServerName::try_from(cfg.host.clone()) {
        Ok(n) => n,
        Err(_) => {
            session::status("error", "That server address isn't valid");
            return End::Stop;
        }
    };
    let tls = match tokio_rustls::TlsConnector::from(Arc::new(tls_cfg)).connect(name, tcp).await {
        Ok(t) => t,
        Err(e) => return End::Retry(format!("Secure connection failed ({e})")),
    };

    // Trust on first use: remember the server's certificate, warn if it ever changes.
    let fp = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| sha1_hex(c.as_ref())).unwrap_or_default();
    let host_key = format!("{}:{}", cfg.host.to_lowercase(), cfg.port);
    let mut known = util::read_json(&known_path()).unwrap_or_else(|| json!({}));
    match known.get(&host_key).and_then(|v| v.as_str()) {
        Some(k) if k != fp && cfg.trust.as_deref() != Some(fp.as_str()) => {
            util::emit("vc-cert", json!({ "host": cfg.host, "port": cfg.port, "fingerprint": fp, "previous": k }));
            session::status("error", "This server's certificate changed. Only continue if you trust it.");
            return End::Stop;
        }
        Some(k) if k == fp => {}
        _ => {
            known[&host_key] = json!(fp);
            util::write_json(&known_path(), &known);
        }
    }

    let (mut rd, mut wr) = tokio::io::split(tls);
    let (wtx, mut wrx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer = tokio::spawn(async move {
        while let Some(b) = wrx.recv().await {
            if wr.write_all(&b).await.is_err() {
                break;
            }
        }
    });
    let (ftx, mut frx) = mpsc::unbounded_channel::<std::io::Result<(u16, Vec<u8>)>>();
    let reader = tokio::spawn(async move {
        loop {
            let r = read_frame(&mut rd).await;
            let stop = r.is_err();
            if ftx.send(r).is_err() || stop {
                break;
            }
        }
    });
    let send = |kind: u16, m: &dyn Fn() -> Vec<u8>| {
        let _ = wtx.send(proto::raw_frame(kind, &m()));
    };

    // We announce 1.4 so every server talks the classic voice format to us.
    send(proto::VERSION, &|| {
        proto::Version {
            version_v1: Some(0x0001_0400),
            version_v2: Some((1u64 << 48) | (4u64 << 32)),
            release: Some(format!("EZ VoiceChat {}", env!("CARGO_PKG_VERSION"))),
            os: Some("Windows".into()),
            os_version: Some(std::env::consts::ARCH.into()),
        }
        .encode_to_vec()
    });
    send(proto::AUTHENTICATE, &|| {
        proto::Authenticate {
            username: Some(cfg.username.clone()),
            password: if cfg.password.is_empty() { None } else { Some(cfg.password.clone()) },
            tokens: vec![],
            celt_versions: vec![],
            opus: Some(true),
            client_type: Some(0),
        }
        .encode_to_vec()
    });

    let udp = match tokio::net::UdpSocket::bind(if addr.is_ipv6() { "[::]:0" } else { "0.0.0.0:0" }).await {
        Ok(u) => {
            let _ = u.connect(addr).await;
            Some(u)
        }
        Err(_) => None,
    };
    let mixer = Arc::new(voice::Mixer::new());
    crate::audio::set_source(Some(mixer.clone()));
    let (vtx, mut vrx) = mpsc::channel::<Vec<u8>>(64);

    let mut view =
        View { kind: "mumble".into(), host: format!("{}:{}", cfg.host, cfg.port), server_name: cfg.host.clone(), codec: "Opus".into(), ..Default::default() };
    let mut crypt = crypt::CryptState::default();
    let mut me: Option<u32> = None;
    let mut synced = false;
    let start = Instant::now();
    let mut last_udp_pong: Option<Instant> = None;
    let mut last_tcp_ping = Instant::now() - Duration::from_secs(60);
    let mut last_udp_ping = Instant::now() - Duration::from_secs(60);
    let mut last_good_udp = Instant::now();
    let mut last_resync = Instant::now();
    let mut bad_udp = 0u32;
    let mut dirty = true;
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    let mut ubuf = vec![0u8; 2048];
    let mut voice_drops = 0u32;

    let result = loop {
        tokio::select! {
            f = frx.recv() => {
                let Some(f) = f else { break End::Retry("Connection lost".into()) };
                let (kind, body) = match f {
                    Ok(x) => x,
                    Err(e) => break End::Retry(if synced { format!("Connection lost ({e})") } else { format!("The server closed the connection ({e})") }),
                };
                match kind {
                    proto::REJECT => {
                        let r = proto::Reject::decode(&body[..]).unwrap_or_default();
                        session::status("error", r.reason.unwrap_or_else(|| "The server refused the connection".into()));
                        break End::Stop;
                    }
                    proto::CRYPT_SETUP => {
                        let m = proto::CryptSetup::decode(&body[..]).unwrap_or_default();
                        match (&m.key, &m.client_nonce, &m.server_nonce) {
                            (Some(k), Some(c), Some(s)) => {
                                crypt.set_key(k, c, s);
                                last_udp_ping = Instant::now() - Duration::from_secs(60);
                            }
                            (_, _, Some(s)) => crypt.set_decrypt_iv(s),
                            _ => {
                                let iv = crypt.encrypt_iv.to_vec();
                                send(proto::CRYPT_SETUP, &|| proto::CryptSetup { key: None, client_nonce: Some(iv.clone()), server_nonce: None }.encode_to_vec());
                            }
                        }
                    }
                    proto::CHANNEL_STATE => {
                        if let Ok(m) = proto::ChannelState::decode(&body[..]) {
                            merge_channel(&mut view, m);
                            dirty = true;
                        }
                    }
                    proto::CHANNEL_REMOVE => {
                        if let Ok(m) = proto::ChannelRemove::decode(&body[..]) {
                            view.channels.remove(&(m.channel_id as u64));
                            dirty = true;
                        }
                    }
                    proto::USER_STATE => {
                        if let Ok(m) = proto::UserState::decode(&body[..]) {
                            merge_user(&mut view, &m);
                            dirty = true;
                        }
                    }
                    proto::USER_REMOVE => {
                        if let Ok(m) = proto::UserRemove::decode(&body[..]) {
                            mixer.remove(m.session);
                            let who = view.users.remove(&(m.session as u64)).map(|u| u.name).unwrap_or_default();
                            dirty = true;
                            if Some(m.session) == me {
                                let reason = m.reason.unwrap_or_default();
                                let what = if m.ban.unwrap_or(false) { "banned" } else { "kicked" };
                                session::status("error", format!("You were {what} from the server{}", if reason.is_empty() { String::new() } else { format!(": {reason}") }));
                                break End::Stop;
                            }
                            if synced && !who.is_empty() {
                                session::chat("", &format!("{who} left"), "system");
                            }
                        }
                    }
                    proto::SERVER_SYNC => {
                        let m = proto::ServerSync::decode(&body[..]).unwrap_or_default();
                        me = m.session;
                        view.me = m.session.map(|s| s as u64);
                        if let Some(w) = m.welcome_text {
                            view.welcome = util::strip_html(&w);
                        }
                        if let Some(root) = view.channels.get(&0) {
                            if !root.name.is_empty() {
                                view.server_name = root.name.clone();
                            }
                        }
                        synced = true;
                        dirty = true;
                        let bitrate = crate::audio::settings().bitrate;
                        crate::audio::set_sink(voice::Sink::new(vtx.clone(), bitrate).map(|s| Box::new(s) as Box<dyn session::VoiceSink>));
                        let (mute, deaf) = crate::audio::self_state();
                        if mute || deaf {
                            let s = me;
                            send(proto::USER_STATE, &|| proto::UserState { session: s, self_mute: Some(mute), self_deaf: Some(deaf), ..Default::default() }.encode_to_vec());
                        }
                        session::status("connected", format!("Connected to {}", view.server_name));
                        if !view.welcome.is_empty() {
                            session::chat("", &view.welcome, "server");
                        }
                    }
                    proto::SERVER_CONFIG => {
                        let m = proto::ServerConfig::decode(&body[..]).unwrap_or_default();
                        if let Some(w) = m.welcome_text {
                            view.welcome = util::strip_html(&w);
                        }
                    }
                    proto::TEXT_MESSAGE => {
                        if let Ok(m) = proto::TextMessage::decode(&body[..]) {
                            let from = m.actor.and_then(|a| view.users.get(&(a as u64))).map(|u| u.name.clone()).unwrap_or_else(|| "Server".into());
                            let private = me.map(|s| m.session.contains(&s)).unwrap_or(false);
                            session::chat(&from, &util::strip_html(&m.message), if private { "private" } else { "channel" });
                        }
                    }
                    proto::PERMISSION_DENIED => {
                        let m = proto::PermissionDenied::decode(&body[..]).unwrap_or_default();
                        let text = m.reason.filter(|r| !r.is_empty()).unwrap_or_else(|| "You don't have permission to do that".into());
                        session::chat("", &util::strip_html(&text), "system");
                    }
                    proto::CODEC_VERSION => {
                        let m = proto::CodecVersion::decode(&body[..]).ok();
                        if m.and_then(|m| m.opus) == Some(false) {
                            session::chat("", "This server doesn't support the Opus codec; others may not hear you.", "system");
                        }
                    }
                    proto::UDP_TUNNEL => {
                        if let voice::Incoming::Audio { session: s, seq, opus, last } = voice::parse_incoming(&body) {
                            mixer.push(s, seq, &opus, last);
                        }
                    }
                    proto::PING => {
                        if let Ok(p) = proto::Ping::decode(&body[..]) {
                            if last_udp_pong.is_none() {
                                if let Some(ts) = p.timestamp {
                                    let now = start.elapsed().as_micros() as u64;
                                    view.ping_ms = Some(now.saturating_sub(ts) as f64 / 1000.0);
                                    dirty = true;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            c = cmds.recv() => {
                match c {
                    None | Some(Cmd::Disconnect) => break End::Stop,
                    Some(Cmd::Join(ch)) => {
                        let s = me;
                        send(proto::USER_STATE, &|| proto::UserState { session: s, channel_id: Some(ch as u32), ..Default::default() }.encode_to_vec());
                    }
                    Some(Cmd::Chat(text)) => {
                        let chan = me.and_then(|s| view.users.get(&(s as u64))).map(|u| u.channel as u32).unwrap_or(0);
                        let html = util::escape_html(&text).replace('\n', "<br>");
                        send(proto::TEXT_MESSAGE, &|| proto::TextMessage { actor: None, session: vec![], channel_id: vec![chan], tree_id: vec![], message: html.clone() }.encode_to_vec());
                        session::chat(&cfg.username, &text, "self");
                    }
                    Some(Cmd::SelfState { mute, deaf }) => {
                        let s = me;
                        send(proto::USER_STATE, &|| proto::UserState { session: s, self_mute: Some(mute), self_deaf: Some(deaf), ..Default::default() }.encode_to_vec());
                    }
                }
            }
            p = vrx.recv() => {
                let Some(p) = p else { continue };
                let udp_ok = last_udp_pong.map(|t| t.elapsed() < Duration::from_secs(12)).unwrap_or(false);
                match (&udp, udp_ok && crypt.is_valid()) {
                    (Some(u), true) => {
                        let enc = crypt.encrypt(&p);
                        if u.send(&enc).await.is_err() {
                            voice_drops += 1;
                        }
                    }
                    _ => {
                        // UDP blocked or not set up yet: tunnel the voice through TCP.
                        let _ = wtx.send(proto::raw_frame(proto::UDP_TUNNEL, &p));
                    }
                }
            }
            r = async {
                match &udp {
                    Some(u) => u.recv(&mut ubuf).await,
                    None => std::future::pending().await,
                }
            } => {
                let Ok(n) = r else { continue };
                match crypt.decrypt(&ubuf[..n]) {
                    Some(plain) => {
                        bad_udp = 0;
                        last_good_udp = Instant::now();
                        match voice::parse_incoming(&plain) {
                            voice::Incoming::Ping(ts) => {
                                let first = last_udp_pong.is_none();
                                last_udp_pong = Some(Instant::now());
                                let now = start.elapsed().as_micros() as u64;
                                view.ping_ms = Some(now.saturating_sub(ts) as f64 / 1000.0);
                                if first || !view.udp {
                                    view.udp = true;
                                }
                                dirty = true;
                            }
                            voice::Incoming::Audio { session: s, seq, opus, last } => mixer.push(s, seq, &opus, last),
                            voice::Incoming::Other => {}
                        }
                    }
                    None => {
                        bad_udp += 1;
                        // Lost sync with the server's packet counter: ask for a fresh IV.
                        if crypt.is_valid() && bad_udp > 20 && last_good_udp.elapsed() > Duration::from_secs(3) && last_resync.elapsed() > Duration::from_secs(5) {
                            last_resync = Instant::now();
                            send(proto::CRYPT_SETUP, &|| proto::CryptSetup::default().encode_to_vec());
                        }
                    }
                }
            }
            _ = tick.tick() => {
                if last_tcp_ping.elapsed() > Duration::from_secs(15) {
                    last_tcp_ping = Instant::now();
                    let ts = start.elapsed().as_micros() as u64;
                    let (good, late, lost, resync) = (crypt.good, crypt.late, crypt.lost, crypt.resync);
                    send(proto::PING, &|| proto::Ping { timestamp: Some(ts), good: Some(good), late: Some(late), lost: Some(lost), resync: Some(resync), ..Default::default() }.encode_to_vec());
                }
                if crypt.is_valid() && last_udp_ping.elapsed() > Duration::from_secs(5) {
                    last_udp_ping = Instant::now();
                    if let Some(u) = &udp {
                        let enc = crypt.encrypt(&voice::ping_packet(start.elapsed().as_micros() as u64));
                        let _ = u.send(&enc).await;
                    }
                }
                if view.udp && last_udp_pong.map(|t| t.elapsed() > Duration::from_secs(12)).unwrap_or(true) {
                    view.udp = false; // UDP stopped answering: voice goes over TCP until it's back
                    dirty = true;
                }
                if dirty && synced && session::generation() == gen {
                    dirty = false;
                    session::set_view(Some(view.clone()));
                }
            }
        }
    };
    let _ = voice_drops;
    crate::audio::set_sink(None);
    crate::audio::set_source(None);
    mixer.clear();
    reader.abort();
    drop(wtx);
    let _ = tokio::time::timeout(Duration::from_millis(300), writer).await;
    if session::generation() == gen {
        session::set_view(None);
    }
    result
}
