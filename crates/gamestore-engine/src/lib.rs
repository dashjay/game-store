//! `gamestore-engine` — general (single-value) storage engine layer.
//!
//! This is the "general engine layer" of [`docs/design/03-storage-engine.md`]
//! §1/§2/§4: it only ever stores the *single, already-merged final value* for a
//! key (multi-version conflict resolution lives above it, in the staging layer
//! introduced in later MRs). It provides:
//!
//! - [`engine`] — the backend-agnostic [`GeneralEngine`] abstraction (plan §2.3):
//!   `get` / `write` (group commit) / `scan_prefix` / `compact_range` /
//!   `install_gc`, plus [`WriteBatch`] and the [`GcPredicate`] trait.
//! - [`rocks`] — the RocksDB-backed [`RocksEngine`] implementation, with tuning
//!   knobs ([`EngineConfig`]: Bloom filter, block cache, rate limiter, write
//!   buffer) and a swappable compaction-filter GC trampoline.
//! - [`encoding`] — the `metadata key + subkey + version` on-disk layout, ported
//!   byte-for-byte from the spike ([`docs/design/03-storage-engine.md`] §2).
//! - [`gc`] — the in-memory `key -> current version` map ([`VersionMap`]) that
//!   drives version-based subkey garbage collection ([`docs/design/03-storage-engine.md`] §4).
//! - [`store`] — the [`Store`]: Redis String/Hash operations encoded onto a
//!   `GeneralEngine`, with lazy expiry and `RAWCOUNT`/`DBSIZE`/`COMPACT`
//!   introspection. Ported and hardened from `spike/rust/src/storage.rs`.
//!
//! The command layer (I-04, `gamestore-datamodel`) parses Redis commands and
//! calls into [`Store`].
#![forbid(unsafe_code)]

pub mod encoding;
pub mod engine;
pub mod error;
pub mod gc;
pub mod rocks;
pub mod store;

pub use encoding::{
    Meta, META_PREFIX, SUBKEY_PREFIX, TYPE_HASH, TYPE_LIST, TYPE_SET, TYPE_STRING, TYPE_ZSET,
    ZSCORE_PREFIX,
};
pub use engine::{GcPredicate, GeneralEngine, Range, ScanItem, WriteBatch, WriteOp};
pub use error::{EngineError, Result};
pub use gc::VersionMap;
pub use rocks::{EngineConfig, RocksEngine};
pub use store::{now_ms, Store};
