//! The Mumble control-channel messages we use (field numbers from Mumble.proto).
//! Written by hand with prost's derive, so the build needs no protoc.
#![allow(clippy::derive_partial_eq_without_eq)]

pub const VERSION: u16 = 0;
pub const UDP_TUNNEL: u16 = 1;
pub const AUTHENTICATE: u16 = 2;
pub const PING: u16 = 3;
pub const REJECT: u16 = 4;
pub const SERVER_SYNC: u16 = 5;
pub const CHANNEL_REMOVE: u16 = 6;
pub const CHANNEL_STATE: u16 = 7;
pub const USER_REMOVE: u16 = 8;
pub const USER_STATE: u16 = 9;
pub const TEXT_MESSAGE: u16 = 11;
pub const PERMISSION_DENIED: u16 = 12;
pub const CRYPT_SETUP: u16 = 15;
pub const PERMISSION_QUERY: u16 = 20;
pub const REQUEST_BLOB: u16 = 23;
pub const CODEC_VERSION: u16 = 21;
pub const SERVER_CONFIG: u16 = 24;

#[derive(Clone, PartialEq, prost::Message)]
pub struct Version {
    #[prost(uint32, optional, tag = "1")]
    pub version_v1: Option<u32>,
    #[prost(string, optional, tag = "2")]
    pub release: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub os: Option<String>,
    #[prost(string, optional, tag = "4")]
    pub os_version: Option<String>,
    #[prost(uint64, optional, tag = "5")]
    pub version_v2: Option<u64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Authenticate {
    #[prost(string, optional, tag = "1")]
    pub username: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub password: Option<String>,
    #[prost(string, repeated, tag = "3")]
    pub tokens: Vec<String>,
    #[prost(int32, repeated, packed = "false", tag = "4")]
    pub celt_versions: Vec<i32>,
    #[prost(bool, optional, tag = "5")]
    pub opus: Option<bool>,
    #[prost(int32, optional, tag = "6")]
    pub client_type: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Ping {
    #[prost(uint64, optional, tag = "1")]
    pub timestamp: Option<u64>,
    #[prost(uint32, optional, tag = "2")]
    pub good: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    pub late: Option<u32>,
    #[prost(uint32, optional, tag = "4")]
    pub lost: Option<u32>,
    #[prost(uint32, optional, tag = "5")]
    pub resync: Option<u32>,
    #[prost(uint32, optional, tag = "6")]
    pub udp_packets: Option<u32>,
    #[prost(uint32, optional, tag = "7")]
    pub tcp_packets: Option<u32>,
    #[prost(float, optional, tag = "8")]
    pub udp_ping_avg: Option<f32>,
    #[prost(float, optional, tag = "9")]
    pub udp_ping_var: Option<f32>,
    #[prost(float, optional, tag = "10")]
    pub tcp_ping_avg: Option<f32>,
    #[prost(float, optional, tag = "11")]
    pub tcp_ping_var: Option<f32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct Reject {
    #[prost(int32, optional, tag = "1")]
    pub r#type: Option<i32>,
    #[prost(string, optional, tag = "2")]
    pub reason: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ServerSync {
    #[prost(uint32, optional, tag = "1")]
    pub session: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub max_bandwidth: Option<u32>,
    #[prost(string, optional, tag = "3")]
    pub welcome_text: Option<String>,
    #[prost(uint64, optional, tag = "4")]
    pub permissions: Option<u64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ChannelRemove {
    #[prost(uint32, required, tag = "1")]
    pub channel_id: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ChannelState {
    #[prost(uint32, optional, tag = "1")]
    pub channel_id: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub parent: Option<u32>,
    #[prost(string, optional, tag = "3")]
    pub name: Option<String>,
    #[prost(uint32, repeated, packed = "false", tag = "4")]
    pub links: Vec<u32>,
    #[prost(string, optional, tag = "5")]
    pub description: Option<String>,
    #[prost(bool, optional, tag = "8")]
    pub temporary: Option<bool>,
    #[prost(int32, optional, tag = "9")]
    pub position: Option<i32>,
    #[prost(uint32, optional, tag = "11")]
    pub max_users: Option<u32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct UserRemove {
    #[prost(uint32, required, tag = "1")]
    pub session: u32,
    #[prost(uint32, optional, tag = "2")]
    pub actor: Option<u32>,
    #[prost(string, optional, tag = "3")]
    pub reason: Option<String>,
    #[prost(bool, optional, tag = "4")]
    pub ban: Option<bool>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct UserState {
    #[prost(uint32, optional, tag = "1")]
    pub session: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub actor: Option<u32>,
    #[prost(string, optional, tag = "3")]
    pub name: Option<String>,
    #[prost(uint32, optional, tag = "4")]
    pub user_id: Option<u32>,
    #[prost(uint32, optional, tag = "5")]
    pub channel_id: Option<u32>,
    #[prost(bool, optional, tag = "6")]
    pub mute: Option<bool>,
    #[prost(bool, optional, tag = "7")]
    pub deaf: Option<bool>,
    #[prost(bool, optional, tag = "8")]
    pub suppress: Option<bool>,
    #[prost(bool, optional, tag = "9")]
    pub self_mute: Option<bool>,
    #[prost(bool, optional, tag = "10")]
    pub self_deaf: Option<bool>,
    #[prost(bytes = "vec", optional, tag = "11")]
    pub texture: Option<Vec<u8>>,
    #[prost(string, optional, tag = "14")]
    pub comment: Option<String>,
    #[prost(string, optional, tag = "15")]
    pub hash: Option<String>,
    #[prost(bytes = "vec", optional, tag = "17")]
    pub texture_hash: Option<Vec<u8>>,
    #[prost(bool, optional, tag = "18")]
    pub priority_speaker: Option<bool>,
    #[prost(bool, optional, tag = "19")]
    pub recording: Option<bool>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct TextMessage {
    #[prost(uint32, optional, tag = "1")]
    pub actor: Option<u32>,
    #[prost(uint32, repeated, packed = "false", tag = "2")]
    pub session: Vec<u32>,
    #[prost(uint32, repeated, packed = "false", tag = "3")]
    pub channel_id: Vec<u32>,
    #[prost(uint32, repeated, packed = "false", tag = "4")]
    pub tree_id: Vec<u32>,
    #[prost(string, required, tag = "5")]
    pub message: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct PermissionDenied {
    #[prost(uint32, optional, tag = "1")]
    pub permission: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub channel_id: Option<u32>,
    #[prost(uint32, optional, tag = "3")]
    pub session: Option<u32>,
    #[prost(string, optional, tag = "4")]
    pub reason: Option<String>,
    #[prost(int32, optional, tag = "5")]
    pub r#type: Option<i32>,
    #[prost(string, optional, tag = "6")]
    pub name: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct CryptSetup {
    #[prost(bytes = "vec", optional, tag = "1")]
    pub key: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "2")]
    pub client_nonce: Option<Vec<u8>>,
    #[prost(bytes = "vec", optional, tag = "3")]
    pub server_nonce: Option<Vec<u8>>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestBlob {
    #[prost(uint32, repeated, packed = "false", tag = "1")]
    pub session_texture: Vec<u32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct PermissionQuery {
    #[prost(uint32, optional, tag = "1")]
    pub channel_id: Option<u32>,
    #[prost(uint32, optional, tag = "2")]
    pub permissions: Option<u32>,
    #[prost(bool, optional, tag = "3")]
    pub flush: Option<bool>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct CodecVersion {
    #[prost(int32, required, tag = "1")]
    pub alpha: i32,
    #[prost(int32, required, tag = "2")]
    pub beta: i32,
    #[prost(bool, required, tag = "3")]
    pub prefer_alpha: bool,
    #[prost(bool, optional, tag = "4")]
    pub opus: Option<bool>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ServerConfig {
    #[prost(uint32, optional, tag = "1")]
    pub max_bandwidth: Option<u32>,
    #[prost(string, optional, tag = "2")]
    pub welcome_text: Option<String>,
    #[prost(bool, optional, tag = "3")]
    pub allow_html: Option<bool>,
    #[prost(uint32, optional, tag = "4")]
    pub message_length: Option<u32>,
    #[prost(uint32, optional, tag = "5")]
    pub image_message_length: Option<u32>,
    #[prost(uint32, optional, tag = "6")]
    pub max_users: Option<u32>,
}

/// One framed control message: u16 type, u32 length (big-endian), payload.
pub fn frame<M: prost::Message>(kind: u16, msg: &M) -> Vec<u8> {
    let body = msg.encode_to_vec();
    raw_frame(kind, &body)
}

pub fn raw_frame(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(6 + body.len());
    v.extend_from_slice(&kind.to_be_bytes());
    v.extend_from_slice(&(body.len() as u32).to_be_bytes());
    v.extend_from_slice(body);
    v
}
