//! The `GeneralEngine` abstraction (plan §2.3).
//!
//! The general engine layer only ever sees the *single, already-merged final
//! value* for a key (multi-version conflict resolution happens above it in the
//! staging layer, see [`docs/design/03-storage-engine.md`] §0). It therefore
//! exposes a deliberately small, backend-agnostic KV surface so RocksDB (today)
//! or an LSH engine (future, plan §7) can be swapped without touching the
//! encoding / version / GC logic layered on top ([`crate::store`]).

use std::sync::Arc;

use crate::error::Result;

/// A batched set of writes applied atomically as one group commit.
///
/// Backend-agnostic on purpose: the RocksDB implementation translates this into
/// a `rocksdb::WriteBatch`, but callers (and the encoding layer) never depend on
/// RocksDB types.
#[derive(Debug, Default, Clone)]
pub struct WriteBatch {
    ops: Vec<WriteOp>,
}

/// A single mutation inside a [`WriteBatch`].
#[derive(Debug, Clone)]
pub enum WriteOp {
    /// Insert or overwrite `key` with `value`.
    Put(Vec<u8>, Vec<u8>),
    /// Delete `key`.
    Delete(Vec<u8>),
}

impl WriteBatch {
    /// Create an empty batch.
    pub fn new() -> Self {
        WriteBatch::default()
    }

    /// Queue a put of `key` → `value`.
    pub fn put(&mut self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) {
        self.ops.push(WriteOp::Put(key.into(), value.into()));
    }

    /// Queue a delete of `key`.
    pub fn delete(&mut self, key: impl Into<Vec<u8>>) {
        self.ops.push(WriteOp::Delete(key.into()));
    }

    /// Number of queued operations.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether the batch has no operations.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Borrow the queued operations (used by concrete engine implementations).
    pub fn ops(&self) -> &[WriteOp] {
        &self.ops
    }
}

/// An inclusive-start, exclusive-end key range for [`GeneralEngine::compact_range`].
///
/// `start = None` means "from the beginning", `end = None` means "to the end".
#[derive(Debug, Default, Clone)]
pub struct Range {
    /// Inclusive lower bound (`None` = unbounded below).
    pub start: Option<Vec<u8>>,
    /// Exclusive upper bound (`None` = unbounded above).
    pub end: Option<Vec<u8>>,
}

/// Predicate consulted by the engine's compaction filter to decide whether a
/// record survives compaction (see [`docs/design/03-storage-engine.md`] §4).
///
/// Implemented by [`crate::gc::VersionMap`]: a subkey is kept iff its owner is
/// still present and the subkey's version equals the owner's current version.
pub trait GcPredicate: Send + Sync {
    /// Return `true` to keep the record, `false` to drop it during compaction.
    fn should_keep(&self, key: &[u8], value: &[u8]) -> bool;
}

/// Boxed key/value pair yielded by [`GeneralEngine::scan_prefix`].
pub type ScanItem = Result<(Vec<u8>, Vec<u8>)>;

/// The pluggable single-value storage engine (plan §2.3).
///
/// Implementations must be cheap to share across threads (`Send + Sync`); the
/// concrete [`crate::rocks::RocksEngine`] is internally `Arc`-friendly.
pub trait GeneralEngine: Send + Sync {
    /// Point lookup. `Ok(None)` if the key is absent.
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>>;

    /// Apply a batch atomically (group commit).
    fn write(&self, batch: WriteBatch) -> Result<()>;

    /// Iterate every record whose key starts with `prefix`, in key order.
    fn scan_prefix<'a>(&'a self, prefix: &[u8]) -> Box<dyn Iterator<Item = ScanItem> + 'a>;

    /// Compact `range` (or the whole keyspace when `None`), forcing the
    /// compaction filter to run so stale records are physically reclaimed.
    fn compact_range(&self, range: Option<Range>) -> Result<()>;

    /// Install (or replace) the GC predicate consulted by the compaction filter.
    fn install_gc(&self, predicate: Arc<dyn GcPredicate>);

    /// Durably persist everything written so far (a checkpoint point): after a
    /// successful `flush`, applied data survives a crash without needing the
    /// WAL, so the WAL prefix covering it can be GC'd
    /// ([`docs/design/03-storage-engine.md`] §6). Backends that are always
    /// durable can keep the default no-op.
    fn flush(&self) -> Result<()> {
        Ok(())
    }

    /// Point-in-time engine statistics as `(metric_name, value)` gauges,
    /// exported through the `/metrics` endpoint (I-07, aligned with
    /// [`docs/design/08-observability-ops.md`] §1.2's engine metrics).
    ///
    /// Names must be valid Prometheus metric names. Backends without
    /// introspection can keep the default empty implementation.
    fn stats(&self) -> Vec<(&'static str, u64)> {
        Vec::new()
    }
}
