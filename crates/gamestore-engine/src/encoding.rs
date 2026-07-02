//! On-disk encoding for the general engine layer.
//!
//! Mirrors [`docs/design/03-storage-engine.md`] §2: every user-visible key has
//! one *metadata* record, and every member of a composite type (Hash/Set/ZSet/
//! List) is one *subkey* record whose key embeds the owner key and its current
//! structure `version`. Deleting/rebuilding a key is an O(1) version bump;
//! stale subkeys (version != current) are reclaimed in the background by the
//! compaction filter (see [`crate::gc`]).
//!
//! The metadata and subkey layouts are a byte-for-byte port of
//! `spike/rust/src/encoding.rs`: data written by the spike (and the C++ spike,
//! which shares the layout) remains readable, per the plan's requirement that
//! the encoding stay byte-for-byte consistent (plan §1, §6). I-06 *adds* new
//! record families on previously-unused prefix/type bytes without touching the
//! existing layouts:
//!
//! - Set members reuse the Hash subkey layout with an empty value (§2.3:
//!   membership is expressed by the key itself);
//! - ZSet uses a **dual encoding** (§2.3): the member subkey stores the raw
//!   score bits, and a second *score-index* record family
//!   ([`ZSCORE_PREFIX`]) is ordered by `(score, member)` for `ZRANGE`/
//!   `ZRANGEBYSCORE` scans;
//! - List elements are subkeys whose field is a fixed-width big-endian index
//!   (§2.3); the metadata payload carries the `[head, tail)` bounds.
//!
//! Key layout
//! ----------
//! ```text
//! metadata   : [META_PREFIX][user_key]
//! subkey     : [SUBKEY_PREFIX][u32 BE key_len][user_key][u64 BE version][field]
//! score index: [ZSCORE_PREFIX][u32 BE key_len][user_key][u64 BE version]
//!              [8-byte order-preserving score][member]
//! ```
//!
//! Metadata value layout
//! ---------------------
//! ```text
//! [type:1][version:u64 BE][expire_ms:u64 BE][payload]
//!   - String payload = raw value bytes
//!   - Hash   payload = [field_count:u32 BE]
//!   - Set    payload = [member_count:u32 BE]
//!   - ZSet   payload = [member_count:u32 BE]
//!   - List   payload = [head:u64 BE][tail:u64 BE]   (elements live in [head, tail))
//! ```

/// Prefix byte identifying a metadata record.
pub const META_PREFIX: u8 = 0x01;
/// Prefix byte identifying a composite-type subkey record.
pub const SUBKEY_PREFIX: u8 = 0x02;
/// Prefix byte identifying a ZSet score-index record (I-06; previously unused).
pub const ZSCORE_PREFIX: u8 = 0x03;

/// Metadata type tag: Redis String.
pub const TYPE_STRING: u8 = 1;
/// Metadata type tag: Redis Hash.
pub const TYPE_HASH: u8 = 2;
/// Metadata type tag: Redis Set (I-06).
pub const TYPE_SET: u8 = 3;
/// Metadata type tag: Redis Sorted Set (I-06).
pub const TYPE_ZSET: u8 = 4;
/// Metadata type tag: Redis List (I-06).
pub const TYPE_LIST: u8 = 5;

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
    parse_versioned(raw, SUBKEY_PREFIX)
}

/// Parse either versioned record family ([`SUBKEY_PREFIX`] or
/// [`ZSCORE_PREFIX`]) into `(user_key, version, rest)`.
///
/// Both families share the `[prefix][u32 BE key_len][user_key][u64 BE
/// version][rest]` shape, so the compaction-filter GC can decide "is this
/// record's version still current?" for both with one parse
/// (see [`crate::gc::VersionMap`]).
pub fn parse_owner_version(raw: &[u8]) -> Option<(&[u8], u64)> {
    let prefix = *raw.first()?;
    if prefix != SUBKEY_PREFIX && prefix != ZSCORE_PREFIX {
        return None;
    }
    parse_versioned(raw, prefix).map(|(uk, ver, _rest)| (uk, ver))
}

fn parse_versioned(raw: &[u8], prefix: u8) -> Option<(&[u8], u64, &[u8])> {
    if raw.is_empty() || raw[0] != prefix || raw.len() < 1 + 4 + 8 {
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
    let rest = &raw[ver_end..];
    Some((user_key, version, rest))
}

// ---- ZSet score index (I-06) -----------------------------------------------

/// Encode an `f64` score into 8 bytes whose unsigned big-endian byte order
/// matches numeric order (standard order-preserving float trick: flip all bits
/// of negative values, flip only the sign bit of non-negative ones).
pub fn encode_score(score: f64) -> [u8; 8] {
    let bits = score.to_bits();
    let ordered = if bits & (1 << 63) != 0 {
        !bits // negative: reverse order and move below positives
    } else {
        bits ^ (1 << 63) // non-negative: shift above negatives
    };
    ordered.to_be_bytes()
}

/// Invert [`encode_score`].
pub fn decode_score(raw: [u8; 8]) -> f64 {
    let ordered = u64::from_be_bytes(raw);
    let bits = if ordered & (1 << 63) != 0 {
        ordered ^ (1 << 63) // was non-negative
    } else {
        !ordered // was negative
    };
    f64::from_bits(bits)
}

/// Prefix shared by every score-index record of `user_key` at `version`.
///
/// Range-scanning this prefix yields members ordered by `(score, member)`,
/// which is exactly the `ZRANGE` / `ZRANGEBYSCORE` iteration order.
pub fn zscore_prefix(user_key: &[u8], version: u64) -> Vec<u8> {
    let mut k = Vec::with_capacity(1 + 4 + user_key.len() + 8);
    k.push(ZSCORE_PREFIX);
    k.extend_from_slice(&(user_key.len() as u32).to_be_bytes());
    k.extend_from_slice(user_key);
    k.extend_from_slice(&version.to_be_bytes());
    k
}

/// Encode the score-index record key for `member` of `user_key` at `version`
/// with `score`: `[zscore_prefix][order-preserving score][member]`.
pub fn zscore_key(user_key: &[u8], version: u64, score: f64, member: &[u8]) -> Vec<u8> {
    let mut k = zscore_prefix(user_key, version);
    k.extend_from_slice(&encode_score(score));
    k.extend_from_slice(member);
    k
}

/// Split the suffix a [`zscore_prefix`] scan yields into `(score, member)`.
///
/// Returns `None` if the suffix is shorter than an encoded score.
pub fn split_score_suffix(suffix: &[u8]) -> Option<(f64, &[u8])> {
    if suffix.len() < 8 {
        return None;
    }
    let score = decode_score(suffix[..8].try_into().ok()?);
    Some((score, &suffix[8..]))
}

// ---- List element index (I-06) ----------------------------------------------

/// Initial head/tail index for a fresh List: the middle of the `u64` space so
/// both `LPUSH` (indexes decreasing) and `RPUSH` (indexes increasing) have
/// effectively unbounded room. Big-endian `u64` fields sort in numeric order,
/// so a subkey prefix scan yields elements left-to-right.
pub const LIST_INITIAL_INDEX: u64 = 1 << 63;

/// Encode a List element index as its fixed-width subkey field.
pub fn list_index_field(index: u64) -> [u8; 8] {
    index.to_be_bytes()
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

    /// Member/field count for counted composite types (Hash/Set/ZSet).
    /// `0` for other types or short payloads.
    pub fn field_count(&self) -> u32 {
        let counted = matches!(self.type_id, TYPE_HASH | TYPE_SET | TYPE_ZSET);
        if counted && self.payload.len() >= 4 {
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

    /// Overwrite the payload with a Hash/Set/ZSet member count.
    pub fn set_field_count(&mut self, n: u32) {
        self.payload = n.to_be_bytes().to_vec();
    }

    /// List `[head, tail)` bounds (elements occupy indexes `head..tail`).
    /// `None` for non-List types or short payloads.
    pub fn list_bounds(&self) -> Option<(u64, u64)> {
        if self.type_id != TYPE_LIST || self.payload.len() < 16 {
            return None;
        }
        let head = u64::from_be_bytes(self.payload[..8].try_into().ok()?);
        let tail = u64::from_be_bytes(self.payload[8..16].try_into().ok()?);
        Some((head, tail))
    }

    /// Overwrite the payload with List `[head, tail)` bounds.
    pub fn set_list_bounds(&mut self, head: u64, tail: u64) {
        let mut p = Vec::with_capacity(16);
        p.extend_from_slice(&head.to_be_bytes());
        p.extend_from_slice(&tail.to_be_bytes());
        self.payload = p;
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

    #[test]
    fn score_encoding_preserves_order() {
        let scores = [
            f64::NEG_INFINITY,
            -1e300,
            -42.5,
            -1.0,
            -f64::MIN_POSITIVE,
            -0.0,
            0.0,
            f64::MIN_POSITIVE,
            0.5,
            1.0,
            42.5,
            1e300,
            f64::INFINITY,
        ];
        for w in scores.windows(2) {
            assert!(
                encode_score(w[0]) <= encode_score(w[1]),
                "byte order violated for {} vs {}",
                w[0],
                w[1]
            );
        }
        for s in scores {
            assert_eq!(decode_score(encode_score(s)), s, "round-trip of {s}");
        }
        // -0.0 and 0.0 compare equal numerically but must both round-trip.
        assert!(decode_score(encode_score(-0.0)).is_sign_negative());
    }

    #[test]
    fn zscore_key_roundtrips_through_prefix_scan_shape() {
        let prefix = zscore_prefix(b"lb", 9);
        let full = zscore_key(b"lb", 9, 3.5, b"alice");
        assert!(full.starts_with(&prefix));
        let (score, member) = split_score_suffix(&full[prefix.len()..]).unwrap();
        assert_eq!(score, 3.5);
        assert_eq!(member, b"alice");
        // GC parse sees the right owner/version for score-index records too.
        assert_eq!(parse_owner_version(&full), Some((&b"lb"[..], 9)));
    }

    #[test]
    fn split_score_suffix_rejects_short_input() {
        assert!(split_score_suffix(b"short").is_none());
    }

    #[test]
    fn parse_owner_version_covers_both_families_and_rejects_meta() {
        let sk = subkey(b"k", 7, b"f");
        assert_eq!(parse_owner_version(&sk), Some((&b"k"[..], 7)));
        assert!(parse_owner_version(&meta_key(b"k")).is_none());
        assert!(parse_owner_version(b"").is_none());
    }

    #[test]
    fn list_index_fields_sort_in_numeric_order() {
        let around = LIST_INITIAL_INDEX;
        let fields = [
            list_index_field(around - 2),
            list_index_field(around - 1),
            list_index_field(around),
            list_index_field(around + 1),
        ];
        for w in fields.windows(2) {
            assert!(w[0] < w[1]);
        }
    }

    #[test]
    fn meta_list_bounds_roundtrip() {
        let mut m = Meta {
            type_id: TYPE_LIST,
            version: 1,
            expire_ms: 0,
            payload: Vec::new(),
        };
        m.set_list_bounds(10, 15);
        assert_eq!(m.list_bounds(), Some((10, 15)));
        let decoded = Meta::decode(&m.encode()).unwrap();
        assert_eq!(decoded.list_bounds(), Some((10, 15)));
        // Non-list types have no bounds.
        let s = Meta {
            type_id: TYPE_STRING,
            version: 1,
            expire_ms: 0,
            payload: vec![0; 16],
        };
        assert_eq!(s.list_bounds(), None);
    }

    #[test]
    fn field_count_covers_set_and_zset() {
        for type_id in [TYPE_HASH, TYPE_SET, TYPE_ZSET] {
            let mut m = Meta {
                type_id,
                version: 1,
                expire_ms: 0,
                payload: Vec::new(),
            };
            m.set_field_count(11);
            assert_eq!(m.field_count(), 11, "type {type_id}");
        }
    }
}
