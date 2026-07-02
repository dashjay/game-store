//! RocksDB-backed [`GeneralEngine`] implementation.
//!
//! This is the concrete engine behind the abstraction in [`crate::engine`]. It
//! ports the RocksDB wiring from `spike/rust/src/storage.rs` (compaction filter
//! for version-based subkey GC) and hardens it:
//!
//! - all operations return [`Result`] instead of panicking,
//! - the compaction filter reads the [`GcPredicate`] from a shared slot so
//!   [`GeneralEngine::install_gc`] can (re)install it after `open` — RocksDB
//!   requires the filter to be registered at open time, so we register a stable
//!   trampoline once and swap the predicate behind it,
//! - tuning knobs (Bloom filter, block cache, rate limiter, write buffer) are
//!   exposed via [`EngineConfig`] (plan §1, [`docs/design/03-storage-engine.md`] §5).

use std::path::Path;
use std::sync::{Arc, RwLock};

use rocksdb::{
    compaction_filter::Decision, BlockBasedOptions, Cache, DBCompactionStyle, Direction,
    IteratorMode, Options, DB,
};

use crate::engine::{GcPredicate, GeneralEngine, Range, ScanItem, WriteBatch, WriteOp};
use crate::error::{EngineError, Result};

/// Shared slot holding the active GC predicate consulted by the compaction
/// filter. `None` means "keep everything" (no GC installed yet).
type GcSlot = RwLock<Option<Arc<dyn GcPredicate>>>;

/// Tuning parameters for the RocksDB engine.
///
/// Defaults are reasonable but intentionally *not* aggressively tuned — plan
/// §1 only asks I-03 to leave the knobs exposed; real tuning happens later with
/// workload data ([`docs/design/03-storage-engine.md`] §5).
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Create the DB if it does not exist. Default `true`.
    pub create_if_missing: bool,
    /// Bloom filter bits per key for point-lookup (`HGET`) acceleration.
    /// `None` disables it. Default `Some(10.0)`.
    pub bloom_bits_per_key: Option<f64>,
    /// Shared block cache capacity in bytes. `None` uses the RocksDB default.
    /// Default `Some(64 MiB)`.
    pub block_cache_bytes: Option<usize>,
    /// Per-column-family write buffer (memtable) size in bytes. `None` uses the
    /// RocksDB default. Default `Some(64 MiB)` to absorb write bursts.
    pub write_buffer_bytes: Option<usize>,
    /// Background I/O rate limit in bytes/sec for flush + compaction, to keep
    /// them from starving foreground writes. `None` disables the limiter.
    /// Default `None`.
    pub rate_limit_bytes_per_sec: Option<i64>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            create_if_missing: true,
            bloom_bits_per_key: Some(10.0),
            block_cache_bytes: Some(64 * 1024 * 1024),
            write_buffer_bytes: Some(64 * 1024 * 1024),
            rate_limit_bytes_per_sec: None,
        }
    }
}

/// RocksDB implementation of [`GeneralEngine`].
pub struct RocksEngine {
    db: DB,
    gc: Arc<GcSlot>,
}

impl RocksEngine {
    /// Open (creating if needed) a RocksDB engine at `path` with `config`.
    ///
    /// The compaction filter is registered here (RocksDB requires it at open
    /// time) as a trampoline that consults the shared GC slot; call
    /// [`GeneralEngine::install_gc`] afterwards to activate version-based GC.
    pub fn open(path: impl AsRef<Path>, config: &EngineConfig) -> Result<RocksEngine> {
        let gc: Arc<GcSlot> = Arc::new(RwLock::new(None));

        let mut opts = Options::default();
        opts.create_if_missing(config.create_if_missing);
        opts.set_compaction_style(DBCompactionStyle::Level);
        if let Some(sz) = config.write_buffer_bytes {
            opts.set_write_buffer_size(sz);
        }
        if let Some(rate) = config.rate_limit_bytes_per_sec {
            // 100ms refill window, default fairness (10) — standard RocksDB knobs.
            opts.set_ratelimiter(rate, 100_000, 10);
        }

        let mut block_opts = BlockBasedOptions::default();
        if let Some(bytes) = config.block_cache_bytes {
            let cache = Cache::new_lru_cache(bytes);
            block_opts.set_block_cache(&cache);
        }
        if let Some(bits) = config.bloom_bits_per_key {
            block_opts.set_bloom_filter(bits, false);
        }
        opts.set_block_based_table_factory(&block_opts);

        // Trampoline: the filter is fixed at open time, but the predicate behind
        // it is swappable via `install_gc`. When no predicate is installed we
        // keep everything, so an engine without GC behaves like a plain KV store.
        let slot = gc.clone();
        opts.set_compaction_filter("gamestore-subkey-gc", move |_level, key, value| {
            let guard = slot.read().expect("gc slot poisoned");
            match guard.as_ref() {
                Some(pred) if !pred.should_keep(key, value) => Decision::Remove,
                _ => Decision::Keep,
            }
        });

        let db = DB::open(&opts, path).map_err(EngineError::from)?;
        Ok(RocksEngine { db, gc })
    }
}

impl GeneralEngine for RocksEngine {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        self.db.get(key).map_err(EngineError::from)
    }

    fn write(&self, batch: WriteBatch) -> Result<()> {
        let mut wb = rocksdb::WriteBatch::default();
        for op in batch.ops() {
            match op {
                WriteOp::Put(k, v) => wb.put(k, v),
                WriteOp::Delete(k) => wb.delete(k),
            }
        }
        self.db.write(wb).map_err(EngineError::from)
    }

    fn scan_prefix<'a>(&'a self, prefix: &[u8]) -> Box<dyn Iterator<Item = ScanItem> + 'a> {
        let prefix = prefix.to_vec();
        let iter = self
            .db
            .iterator(IteratorMode::From(&prefix, Direction::Forward));
        Box::new(PrefixScan {
            iter,
            prefix,
            done: false,
        })
    }

    fn compact_range(&self, range: Option<Range>) -> Result<()> {
        // The filter only sees SST data, not the memtable, so flush first.
        self.db.flush().map_err(EngineError::from)?;
        let mut copts = rocksdb::CompactOptions::default();
        // Force the bottommost level so RocksDB cannot skip the rewrite via a
        // trivial move, guaranteeing the filter actually runs (and GC happens).
        copts.set_bottommost_level_compaction(rocksdb::BottommostLevelCompaction::Force);
        let (start, end) = match range {
            Some(r) => (r.start, r.end),
            None => (None, None),
        };
        self.db
            .compact_range_opt(start.as_deref(), end.as_deref(), &copts);
        Ok(())
    }

    fn install_gc(&self, predicate: Arc<dyn GcPredicate>) {
        *self.gc.write().expect("gc slot poisoned") = Some(predicate);
    }

    /// Flush memtables to SST files so applied data is durable on disk
    /// independently of any WAL (used as a checkpoint before WAL truncation).
    fn flush(&self) -> Result<()> {
        self.db.flush().map_err(EngineError::from)
    }

    /// RocksDB properties exported as gauges, chosen to cover the engine
    /// signals of [`docs/design/08-observability-ops.md`] §1.2: block-cache
    /// usage, write-stall state, memtable/compaction pressure and on-disk
    /// footprint. Missing properties are silently skipped.
    fn stats(&self) -> Vec<(&'static str, u64)> {
        const PROPS: &[(&str, &str)] = &[
            ("rocksdb_estimate_num_keys", "rocksdb.estimate-num-keys"),
            (
                "rocksdb_block_cache_usage_bytes",
                "rocksdb.block-cache-usage",
            ),
            (
                "rocksdb_cur_size_all_mem_tables_bytes",
                "rocksdb.cur-size-all-mem-tables",
            ),
            (
                "rocksdb_total_sst_files_size_bytes",
                "rocksdb.total-sst-files-size",
            ),
            (
                "rocksdb_estimate_pending_compaction_bytes",
                "rocksdb.estimate-pending-compaction-bytes",
            ),
            (
                "rocksdb_num_running_compactions",
                "rocksdb.num-running-compactions",
            ),
            ("rocksdb_num_running_flushes", "rocksdb.num-running-flushes"),
            (
                "rocksdb_actual_delayed_write_rate",
                "rocksdb.actual-delayed-write-rate",
            ),
            ("rocksdb_is_write_stopped", "rocksdb.is-write-stopped"),
        ];
        PROPS
            .iter()
            .filter_map(|(name, prop)| {
                self.db
                    .property_int_value(*prop)
                    .ok()
                    .flatten()
                    .map(|v| (*name, v))
            })
            .collect()
    }
}

/// Iterator adapter that stops once keys leave `prefix`.
struct PrefixScan<'a> {
    iter: rocksdb::DBIteratorWithThreadMode<'a, DB>,
    prefix: Vec<u8>,
    done: bool,
}

impl Iterator for PrefixScan<'_> {
    type Item = ScanItem;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.iter.next()? {
            Ok((k, v)) => {
                if k.starts_with(&self.prefix) {
                    Some(Ok((k.to_vec(), v.to_vec())))
                } else {
                    self.done = true;
                    None
                }
            }
            Err(e) => {
                self.done = true;
                Some(Err(EngineError::from(e)))
            }
        }
    }
}
