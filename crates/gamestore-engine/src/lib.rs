//! `gamestore-engine` — general (single-value) storage engine layer.
//!
//! Skeleton crate introduced in **I-01**. The `GeneralEngine` trait, the
//! RocksDB-backed implementation and the `metadata key + subkey + version`
//! encoding with compaction-filter GC (ported from `spike/rust/src/{encoding,
//! gc,storage}.rs`, kept byte-for-byte identical) land in **I-03**. Left empty
//! for now to keep the workspace buildable and boundaries aligned with the
//! plan (§2.1).
#![forbid(unsafe_code)]

/// Crate name, exposed for wiring/smoke assertions until the engine API exists.
pub const CRATE_NAME: &str = "gamestore-engine";
