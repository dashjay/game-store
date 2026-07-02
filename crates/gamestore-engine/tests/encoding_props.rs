//! Property tests for the on-disk encoding (plan I-03 DoD: encoding round-trip).

use gamestore_engine::encoding::{self, Meta, TYPE_HASH, TYPE_STRING};
use proptest::prelude::*;

proptest! {
    /// Metadata records survive an encode → decode round-trip unchanged.
    #[test]
    fn meta_roundtrip(
        type_id in prop_oneof![Just(TYPE_STRING), Just(TYPE_HASH)],
        version in any::<u64>(),
        expire_ms in any::<u64>(),
        payload in proptest::collection::vec(any::<u8>(), 0..256),
    ) {
        let meta = Meta { type_id, version, expire_ms, payload };
        let decoded = Meta::decode(&meta.encode()).expect("decodes");
        prop_assert_eq!(decoded, meta);
    }

    /// Subkeys survive a build → parse round-trip, and the shared prefix is
    /// always a prefix of the full subkey (so HGETALL range scans are correct).
    #[test]
    fn subkey_roundtrip(
        user_key in proptest::collection::vec(any::<u8>(), 0..64),
        version in any::<u64>(),
        field in proptest::collection::vec(any::<u8>(), 0..64),
    ) {
        let raw = encoding::subkey(&user_key, version, &field);
        let (uk, ver, f) = encoding::parse_subkey(&raw).expect("well-formed");
        prop_assert_eq!(uk, &user_key[..]);
        prop_assert_eq!(ver, version);
        prop_assert_eq!(f, &field[..]);

        let prefix = encoding::subkey_prefix(&user_key, version);
        prop_assert!(raw.starts_with(&prefix));
        prop_assert_eq!(&raw[prefix.len()..], &field[..]);
    }

    /// parse_subkey never panics on arbitrary bytes (robustness / anti-abuse).
    #[test]
    fn parse_subkey_never_panics(bytes in proptest::collection::vec(any::<u8>(), 0..128)) {
        let _ = encoding::parse_subkey(&bytes);
    }
}
