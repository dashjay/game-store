//! [`WalEngine`]: a [`GeneralEngine`] decorator that logs every write to a
//! [`Wal`] before applying it — the "write WAL first, then the engine" path of
//! `docs/design/03-storage-engine.md` §8.
//!
//! Wrapping the engine at the [`GeneralEngine::write`] choke point keeps the
//! layering clean: the [`Store`](gamestore_engine::Store) and the whole
//! command layer above it are untouched, and every mutation — regardless of
//! which command produced it — funnels through one durable path. `get` /
//! `scan_prefix` / `compact_range` / `install_gc` delegate straight to the
//! inner engine.
//!
//! # Write path
//!
//! `write(batch)` → append the batch as one [`WalRecord`] → `fsync` (coalesced
//! with concurrent writers, see [`crate::file`]) → apply to the inner engine.
//! Only after the `fsync` returns is the write durable and safe to apply, so a
//! crash between the `fsync` and the engine apply is recovered by replay.
//!
//! # Recovery & checkpointing
//!
//! On [`WalEngine::recovered`] the log is replayed into the inner engine before
//! anything else reads it — re-applying the physical `Put`/`Delete` records is
//! idempotent, so replaying writes the engine already has is harmless. Once the
//! retained log grows past `checkpoint_bytes`, a checkpoint flushes the inner
//! engine (making applied data durable there) and truncates the now-redundant
//! log prefix (`docs/design/03-storage-engine.md` §6: "once flushed to the
//! general engine, the WAL can be GC'd").

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use gamestore_engine::{GcPredicate, GeneralEngine, Range, Result, ScanItem, WriteBatch, WriteOp};

use crate::record::{WalOp, WalRecord};
use crate::wal::Wal;

/// A [`GeneralEngine`] that write-ahead-logs to a [`Wal`] before applying.
pub struct WalEngine<E: GeneralEngine> {
    inner: E,
    wal: Arc<dyn Wal>,
    checkpoint_bytes: u64,
    /// Highest LSN whose effects are known-applied to the inner engine.
    applied_lsn: AtomicU64,
    /// Held while a checkpoint runs so only one happens at a time.
    checkpoint_guard: Mutex<()>,
}

impl<E: GeneralEngine> WalEngine<E> {
    /// Wrap `inner` with `wal`, **replaying** the log into `inner` first so the
    /// engine reflects every durably-logged write before it is read.
    ///
    /// `checkpoint_bytes` is the retained-log size that triggers a checkpoint
    /// (engine flush + log truncation).
    pub fn recovered(inner: E, wal: Arc<dyn Wal>, checkpoint_bytes: u64) -> Result<Self> {
        let replayed = wal.replay(1).map_err(gamestore_engine::EngineError::from)?;
        let mut last_lsn = 0;
        let mut count = 0usize;
        for entry in &replayed {
            inner.write(record_to_batch(&entry.record))?;
            last_lsn = last_lsn.max(entry.lsn);
            count += 1;
        }
        if count > 0 {
            tracing::info!(
                records = count,
                through_lsn = last_lsn,
                "replayed WAL into engine"
            );
        }
        // Everything on disk is already applied to `inner` now.
        let applied = last_lsn.max(wal.next_lsn().saturating_sub(1));
        Ok(WalEngine {
            inner,
            wal,
            checkpoint_bytes: checkpoint_bytes.max(1),
            applied_lsn: AtomicU64::new(applied),
            checkpoint_guard: Mutex::new(()),
        })
    }

    /// Borrow the inner engine (for advanced callers / benchmarks).
    pub fn inner(&self) -> &E {
        &self.inner
    }

    /// Borrow the shared WAL handle.
    pub fn wal(&self) -> &Arc<dyn Wal> {
        &self.wal
    }

    /// Force a checkpoint: flush the inner engine, then GC the log prefix whose
    /// effects are now durable in the engine. Called on graceful shutdown and
    /// implicitly once the retained log exceeds `checkpoint_bytes`.
    pub fn checkpoint(&self) -> Result<()> {
        // Skip if another checkpoint is already in progress.
        let Ok(_guard) = self.checkpoint_guard.try_lock() else {
            return Ok(());
        };
        let upto = self.applied_lsn.load(Ordering::Acquire);
        self.inner.flush()?;
        // Discard records strictly below `upto + 1` (i.e. up to and including
        // `upto`), which are all durably applied to the engine.
        self.wal
            .truncate(upto + 1)
            .map_err(gamestore_engine::EngineError::from)?;
        Ok(())
    }

    fn maybe_checkpoint(&self) -> Result<()> {
        if self.wal.pending_bytes() >= self.checkpoint_bytes {
            self.checkpoint()?;
        }
        Ok(())
    }
}

impl<E: GeneralEngine> GeneralEngine for WalEngine<E> {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.inner.get(key)
    }

    fn write(&self, batch: WriteBatch) -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let record = batch_to_record(&batch);
        // Durable-before-apply: append, fsync (group-committed), then apply.
        let lsn = self
            .wal
            .append(std::slice::from_ref(&record))
            .map_err(gamestore_engine::EngineError::from)?;
        self.wal
            .sync()
            .map_err(gamestore_engine::EngineError::from)?;
        self.inner.write(batch)?;
        // The record is now applied to the inner engine.
        bump_max(&self.applied_lsn, lsn);
        self.maybe_checkpoint()
    }

    fn scan_prefix<'a>(&'a self, prefix: &[u8]) -> Box<dyn Iterator<Item = ScanItem> + 'a> {
        self.inner.scan_prefix(prefix)
    }

    fn compact_range(&self, range: Option<Range>) -> Result<()> {
        self.inner.compact_range(range)
    }

    fn install_gc(&self, predicate: Arc<dyn GcPredicate>) {
        self.inner.install_gc(predicate);
    }

    fn flush(&self) -> Result<()> {
        self.inner.flush()
    }

    /// Inner-engine stats plus the `wal_gc_pending` gauge (retained log bytes
    /// awaiting GC, `docs/design/08-observability-ops.md` §1.2).
    fn stats(&self) -> Vec<(&'static str, u64)> {
        let mut out = self.inner.stats();
        out.push(("wal_gc_pending", self.wal.pending_bytes()));
        out
    }
}

fn bump_max(cell: &AtomicU64, value: u64) {
    let mut cur = cell.load(Ordering::Acquire);
    while value > cur {
        match cell.compare_exchange_weak(cur, value, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => break,
            Err(observed) => cur = observed,
        }
    }
}

fn batch_to_record(batch: &WriteBatch) -> WalRecord {
    let ops = batch
        .ops()
        .iter()
        .map(|op| match op {
            WriteOp::Put(k, v) => WalOp::Put(k.clone(), v.clone()),
            WriteOp::Delete(k) => WalOp::Delete(k.clone()),
        })
        .collect();
    WalRecord::new(ops)
}

fn record_to_batch(record: &WalRecord) -> WriteBatch {
    let mut batch = WriteBatch::new();
    for op in &record.ops {
        match op {
            WalOp::Put(k, v) => batch.put(k.clone(), v.clone()),
            WalOp::Delete(k) => batch.delete(k.clone()),
        }
    }
    batch
}
