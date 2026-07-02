//! The DataNode's logical **Core**: a store fronted by a per-Core WAL (I-08).
//!
//! Per plan §1 and the note in [`crate::server`], a DataNode today runs exactly
//! one logical Core. As of I-08 that Core is `store + WAL`: the RocksDB engine
//! wrapped in a [`WalEngine`] so every write is logged and `fsync`'d before it
//! is applied ("write WAL first, then engine", `docs/design/03-storage-engine.md`
//! §8). The `Arc<CoreStore>` shared across connections *is* that Core unit.
//!
//! The design's "multiple Replicas per Core share one WAL" is left as an
//! interface seam — the WAL is an `Arc<dyn Wal>` and each [`WalRecord`] carries
//! a reserved partition id — so growing to a `Vec<Core>` routed by partition
//! (Phase-2 replication MRs) needs no format change here.

use std::path::Path;
use std::sync::Arc;

use gamestore_common::WalSettings;
use gamestore_engine::{EngineConfig, RocksEngine, Store};
use gamestore_wal::{FileWal, NullWal, Wal, WalConfig, WalEngine};

/// The engine type behind a Core: RocksDB with a WAL on its write path.
pub type CoreEngine = WalEngine<RocksEngine>;

/// The store type shared across all connections on this DataNode.
pub type CoreStore = Store<CoreEngine>;

/// Open the DataNode's Core under `data_dir`: RocksDB at `data_dir/engine`,
/// the WAL at `data_dir/wal`.
///
/// When `wal.enabled`, a [`FileWal`] provides crash durability and its log is
/// replayed into the engine before the store is served. When disabled, a
/// [`NullWal`] keeps the same code path with no durability (benchmarking only).
pub fn open_core(
    data_dir: &Path,
    engine_config: &EngineConfig,
    wal: &WalSettings,
) -> anyhow::Result<Arc<CoreStore>> {
    let engine_dir = data_dir.join("engine");
    std::fs::create_dir_all(&engine_dir)?;
    // Our WAL is the authoritative durability layer, so turn RocksDB's own WAL
    // off to avoid double-logging every write (I-08). Applied data becomes
    // crash-durable in the engine at checkpoint (flush) time, before the WAL
    // prefix covering it is truncated.
    let engine_config = EngineConfig {
        disable_rocksdb_wal: true,
        ..engine_config.clone()
    };
    let rocks = RocksEngine::open(&engine_dir, &engine_config)?;

    let wal_handle: Arc<dyn Wal> = if wal.enabled {
        let wal_dir = data_dir.join("wal");
        Arc::new(FileWal::open(
            &wal_dir,
            &WalConfig {
                segment_max_bytes: wal.segment_max_bytes,
            },
        )?)
    } else {
        tracing::warn!("WAL disabled ([wal] enabled = false): writes are NOT crash-durable");
        Arc::new(NullWal::new())
    };

    let engine = WalEngine::recovered(rocks, wal_handle, wal.checkpoint_bytes)?;
    let store = Store::with_engine(engine)?;
    Ok(Arc::new(store))
}
