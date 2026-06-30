//! On-disk encoding for the general engine layer.
//!
//! Mirrors `docs/design/03-storage-engine.md` §2: every user-visible key has one
//! *metadata* record, and every member of a composite type (Hash) is one
//! *subkey* record whose key embeds the owner key and its current structure
//! `version`. Deleting/rebuilding a key is an O(1) version bump; stale subkeys
//! (version != current) are reclaimed in the background by the compaction
//! filter (see `gc.rs`).
//!
//! Key layout
//! ----------
//! metadata: [META_PREFIX][user_key]
//! subkey  : [SUBKEY_PREFIX][u32 BE key_len][user_key][u64 BE version][field]
//!
//! Metadata value layout
//! ---------------------
//! [type:1][version:u64 BE][expire_ms:u64 BE][payload]
//!   - String payload = raw value bytes
//!   - Hash   payload = [field_count:u32 BE]

pub const META_PREFIX: u8 = 0x01;
pub const SUBKEY_PREFIX: u8 = 0x02;

pub const TYPE_STRING: u8 = 1;
pub const TYPE_HASH: u8 = 2;

pub const META_HEADER_LEN: usize = 1 + 8 + 8; // type + version + expire_ms

pub fn meta_key(user_key: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(1 + user_key.len());
    k.push(META_PREFIX);
    k.extend_from_slice(user_key);
    k
}

/// Prefix shared by every subkey of `user_key` at a given `version`.
/// Used both for point writes and for range scans (HGETALL).
pub fn subkey_prefix(user_key: &[u8], version: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(1 + 4 + user_key.len() + 8);
    k.push(SUBKEY_PREFIX);
    k.extend_from_slice(&(user_key.len() as u32).to_be_bytes());
    k.extend_from_slice(user_key);
    k.extend_from_slice(&version.to_be_bytes());
    k
}

pub fn subkey(user_key: &[u8], version: u64, field: &[u8]) -> Vec<u8> {
    let mut k = subkey_prefix(user_key, version);
    k.extend_from_slice(field);
    k
}

/// Parse a raw subkey record key into (user_key, version, field).
/// Returns None if the bytes are not a well-formed subkey.
pub fn parse_subkey(raw: &[u8]) -> Option<(&[u8], u64, &[u8])> {
    if raw.is_empty() || raw[0] != SUBKEY_PREFIX || raw.len() < 1 + 4 + 8 {
        return None;
    }
    let klen = u32::from_be_bytes(raw[1..5].try_into().ok()?) as usize;
    let key_start = 5;
    let key_end = key_start + klen;
    let ver_end = key_end + 8;
    if raw.len() < ver_end {
        return None;
    }
    let user_key = &raw[key_start..key_end];
    let version = u64::from_be_bytes(raw[key_end..ver_end].try_into().ok()?);
    let field = &raw[ver_end..];
    Some((user_key, version, field))
}

/// A decoded metadata record.
pub struct Meta {
    pub type_id: u8,
    pub version: u64,
    pub expire_ms: u64,
    pub payload: Vec<u8>,
}

impl Meta {
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(META_HEADER_LEN + self.payload.len());
        v.push(self.type_id);
        v.extend_from_slice(&self.version.to_be_bytes());
        v.extend_from_slice(&self.expire_ms.to_be_bytes());
        v.extend_from_slice(&self.payload);
        v
    }

    pub fn decode(raw: &[u8]) -> Option<Meta> {
        if raw.len() < META_HEADER_LEN {
            return None;
        }
        Some(Meta {
            type_id: raw[0],
            version: u64::from_be_bytes(raw[1..9].try_into().ok()?),
            expire_ms: u64::from_be_bytes(raw[9..17].try_into().ok()?),
            payload: raw[META_HEADER_LEN..].to_vec(),
        })
    }

    pub fn field_count(&self) -> u32 {
        if self.type_id == TYPE_HASH && self.payload.len() >= 4 {
            u32::from_be_bytes([self.payload[0], self.payload[1], self.payload[2], self.payload[3]])
        } else {
            0
        }
    }

    pub fn set_field_count(&mut self, n: u32) {
        self.payload = n.to_be_bytes().to_vec();
    }
}
