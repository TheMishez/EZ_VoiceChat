//! Profile pictures. Yours is a small JPEG (made 128×128 by the page) saved in the app's data
//! folder and sent to Mumble servers as your "texture", so everyone on the server sees it.
//! Pictures from other people are kept in memory by content hash; the channel tree and the
//! overlay ask for them by that hash, so the big image data never rides along with every state
//! update.
use crate::session::{self, Cmd};
use crate::util;
use base64::Engine;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;

/// Largest picture we send or accept (bytes).
pub const MAX_BYTES: usize = 48 * 1024;

static CACHE: Mutex<Option<HashMap<String, String>>> = parking_lot::const_mutex(None);

fn path() -> std::path::PathBuf {
    util::data_dir().join("avatar.jpg")
}

fn mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(b"GIF8") {
        Some("image/gif")
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// SHA-1 in hex: Mumble identifies pictures by SHA-1, so a known hash needs no download.
pub fn hash_hex(b: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, b).as_ref().iter().map(|x| format!("{x:02x}")).collect()
}

/// Keeps a picture someone has (if it's a normal image) and returns its hash, or "" if not.
pub fn remember(b: &[u8]) -> String {
    if b.is_empty() || b.len() > 256 * 1024 {
        return String::new();
    }
    let Some(m) = mime(b) else { return String::new() };
    let h = hash_hex(b);
    let mut c = CACHE.lock();
    let map = c.get_or_insert_with(HashMap::new);
    if map.len() > 500 {
        map.clear();
    }
    map.entry(h.clone()).or_insert_with(|| format!("data:{m};base64,{}", base64::engine::general_purpose::STANDARD.encode(b)));
    h
}

pub fn known(hash: &str) -> bool {
    CACHE.lock().as_ref().is_some_and(|m| m.contains_key(hash))
}

/// Pictures for the page, by hash.
pub fn get(hashes: &[String]) -> Value {
    let c = CACHE.lock();
    let mut out = serde_json::Map::new();
    if let Some(map) = c.as_ref() {
        for h in hashes.iter().take(200) {
            if let Some(u) = map.get(h) {
                out.insert(h.clone(), json!(u));
            }
        }
    }
    Value::Object(out)
}

/// Your own picture, if you set one.
pub fn own() -> Option<Vec<u8>> {
    std::fs::read(path()).ok().filter(|b| !b.is_empty() && b.len() <= MAX_BYTES && mime(b).is_some())
}

pub fn own_data_url() -> Option<String> {
    let b = own()?;
    let h = remember(&b);
    CACHE.lock().as_ref().and_then(|m| m.get(&h).cloned())
}

/// Sets (a data: URL from the page) or removes (None) your picture, and updates the server
/// you're connected to.
pub fn set_own(data_url: Option<String>) -> Result<Option<String>, String> {
    match data_url {
        None => {
            let _ = std::fs::remove_file(path());
            session::send(Cmd::Avatar(Vec::new()));
            Ok(None)
        }
        Some(u) => {
            let b64 = u.split_once(',').map(|x| x.1).unwrap_or(&u);
            let b = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|_| "That picture couldn't be read.".to_string())?;
            if b.len() > MAX_BYTES {
                return Err("That picture is too large.".into());
            }
            if mime(&b).is_none() {
                return Err("Use a JPG, PNG, GIF or WebP picture.".into());
            }
            std::fs::write(path(), &b).map_err(|e| e.to_string())?;
            let h = remember(&b);
            session::send(Cmd::Avatar(b));
            Ok(Some(h))
        }
    }
}
