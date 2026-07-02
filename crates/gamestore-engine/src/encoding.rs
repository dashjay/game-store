//! On-disk encoding for the general engine layer.
//!
//! Mirrors [`docs/design/03-storage-engine.md`] §2: every user-visible key has
//! one *metadata* record, and every member of a composite type (Hash) is one
//! *subkey* record whose key embeds the owner key and its current structure
//! `version`. Deleting/rebuilding a key is an O(1) version bump; stale subkeys
//! (version != current) are reclaimed in the background by the compaction
//! filter (see [`crate::gc`]).
//!
//! This module is a byte-for-byte port of `spike/rust/src/encoding.rs`: the
//! disk layout is kept **identical** so data written by the spike (and the C++
//! spike, which shares the layout) remains readable, per the plan's requirement
//! that the encoding stay byte-for-byte consistent (plan §1, §6).
//!
//! Key layout
//! ----------
//! ```text
//! metadata: [META_PREFIX][user_key]
//! subkey  : [SUBKEY_PREFIX][u32 BE key_len][user_key][u64 BE version][field]
//! ```
//!
//! Metadata value layout
//! ---------------------
//! ```text
//! [type:1][version:u64 BE][expire_ms:u64 BE][payload]
//!   - String payload = raw value bytes
//!   - Hash   payload = [field_count:u32 BE]
//! ```

/// Prefix byte identifying a metadata record.
pub const META_PREFIX: u8 = 0x01;
/// Prefix byte identifying a composite-type subkey record.
pub const SUBKEY_PREFIX: u8 = 0x02;

/// Metadata type tag: Redis String.
pub const TYPE_STRING: u8 = 1;
/// Metadata type tag: Redis Hash.
pub const TYPE_HASH: u8 = 2;

/// Length of the fixed metadata header: `type(1) + version(8) + expire_ms(8)`.
pub const META_HEADER_LEN: usize = 1 + 8 + 8;

/// Encode the metadata key for `user_key`: `[META_PREFIX][user_key]`.
pub fn meta_key(user_key: &[u8]) -> Vec<u8> {
    let mut k = Vec::with_capacity(1 + user_key.len());
    k.push(META_PREFIX);
    k.extend_from_slice(user_key);
    k
}

/// Prefix shared by every subkey of `user_key` at a given `version`.
///
/// Used both for point writes and for range scans (`HGETALL`).
pub fn subkey_prefix(user_key: &[u8], version: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(1 + 4 + user_key.len() + 8);
    k.push(SUBKEY_PREFIX);
    k.extend_from_slice(&(user_key.len() as u32).to_be_bytes());
    k.extend_from_slice(user_key);
    k.extend_from_slice(&version.to_be_bytes());
    k
}

/// Encode the subkey record key for `field` of `user_key` at `version`.
pub fn subkey(user_key: &[u8], version: u64, field: &[u8]) -> Vec<u8> {
    let mut k = subkey_prefix(user_key, version);
    k.extend_from_slice(field);
    k
}

/// Parse a raw subkey record key into `(user_key, version, field)`.
///
/// Returns `None` if the bytes are not a well-formed subkey (wrong prefix, too
/// short, or an inconsistent embedded length).
pub fn parse_subkey(raw: &[u8]) -> Option<(&[u8], u64, &[u8])> {
    if raw.is_empty() || raw[0] != SUBKEY_PREFIX || raw.len() < 1 + 4 + 8 {
        return None;
    }
    let klen = u32::from_be_bytes(raw[1..5].try_into().ok()?) as usize;
    let key_start: usize = 5;
    let key_end = key_start.checked_add(klen)?;
    let ver_end = key_end.checked_add(8)?;
    if raw.len() < ver_end {
        return None;
    }
    let user_key = &raw[key_start..key_end];
    let version = u64::from_be_bytes(raw[key_end..ver_end].try_into().ok()?);
    let field = &raw[ver_end..];
    Some((user_key, version, field))
}

/// A decoded metadata record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// Redis type tag (see [`TYPE_STRING`] / [`TYPE_HASH`]).
    pub type_id: u8,
    /// Structure version (bumped on delete/rebuild for O(1) logical delete).
    pub version: u64,
    /// Absolute expiry in unix-epoch milliseconds; `0` means "no expiry".
    pub expire_ms: u64,
    /// Type-specific payload (String value bytes, or Hash `field_count`).
    pub payload: Vec<u8>,
}

impl Meta {
    /// Serialize this metadata record to its on-disk byte layout.
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(META_HEADER_LEN + self.payload.len());
        v.push(self.type_id);
        v.extend_from_slice(&self.version.to_be_bytes());
        v.extend_from_slice(&self.expire_ms.to_be_bytes());
        v.extend_from_slice(&self.payload);
        v
    }

    /// Parse a metadata record from raw bytes, or `None` if too short.
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

    /// Hash field count (0 for non-hash or short payloads).
    pub fn field_count(&self) -> u32 {
        if self.type_id == TYPE_HASH && self.payload.len() >= 4 {
            u32::from_be_bytes([
                self.payload[0],
                self.payload[1],
                self.payload[2],
                self.payload[3],
            ])
        } else {
            0
        }
    }

    /// Overwrite the payload with a Hash field count.
    pub fn set_field_count(&mut self, n: u32) {
        self.payload = n.to_be_bytes().to_vec();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_key_has_prefix() {
        assert_eq!(
            meta_key(b"player:1"),
            [&[META_PREFIX][..], b"player:1"].concat()
        );
    }

    #[test]
    fn subkey_roundtrips() {
        let raw = subkey(b"player:1", 42, b"gold");
        let (uk, ver, field) = parse_subkey(&raw).expect("well-formed subkey");
        assert_eq!(uk, b"player:1");
        assert_eq!(ver, 42);
        assert_eq!(field, b"gold");
    }

    #[test]
    fn subkey_prefix_is_prefix_of_subkey() {
        let prefix = subkey_prefix(b"k", 7);
        let full = subkey(b"k", 7, b"field");
        assert!(full.starts_with(&prefix));
        assert_eq!(&full[prefix.len()..], b"field");
    }

    #[test]
    fn parse_subkey_rejects_metadata_and_garbage() {
        assert!(parse_subkey(&meta_key(b"k")).is_none());
        assert!(parse_subkey(b"").is_none());
        assert!(parse_subkey(&[SUBKEY_PREFIX]).is_none());
        // Declared key length longer than the buffer must not panic / must reject.
        let mut bad = vec![SUBKEY_PREFIX];
        bad.extend_from_slice(&u32::MAX.to_be_bytes());
        bad.extend_from_slice(&0u64.to_be_bytes());
        assert!(parse_subkey(&bad).is_none());
    }

    #[test]
    fn empty_field_and_key_are_wellformed() {
        let raw = subkey(b"", 0, b"");
        let (uk, ver, field) = parse_subkey(&raw).expect("empty key/field is valid");
        assert_eq!(uk, b"");
        assert_eq!(ver, 0);
        assert_eq!(field, b"");
    }

    #[test]
    fn meta_roundtrips_string_and_hash() {
        let s = Meta {
            type_id: TYPE_STRING,
            version: 123,
            expire_ms: 456,
            payload: b"value".to_vec(),
        };
        assert_eq!(Meta::decode(&s.encode()), Some(s));

        let mut h = Meta {
            type_id: TYPE_HASH,
            version: 999,
            expire_ms: 0,
            payload: Vec::new(),
        };
        h.set_field_count(7);
        let decoded = Meta::decode(&h.encode()).unwrap();
        assert_eq!(decoded.field_count(), 7);
        assert_eq!(decoded, h);
    }

    #[test]
    fn meta_decode_rejects_short_buffer() {
        assert!(Meta::decode(&[0u8; META_HEADER_LEN - 1]).is_none());
    }
}
