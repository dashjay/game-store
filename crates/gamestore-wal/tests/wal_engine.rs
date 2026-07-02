//! Integration tests for [`WalEngine`]: the "log then apply" write path, crash
//! recovery into a fresh engine, idempotent replay, and checkpoint-bounded log
//! growth. Uses an in-memory mock [`GeneralEngine`] so the assertions are about
//! the WAL logic, not RocksDB (the RocksDB-backed end-to-end restart lives in
//! the datanode tests).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use gamestore_engine::{GcPredicate, GeneralEngine, Range, Result, ScanItem, WriteBatch, WriteOp};
use gamestore_wal::{FileWal, Wal, WalConfig, WalEngine};
use tempfile::TempDir;

/// A trivial in-memory ordered KV engine. Its map can be shared/cloned to model
/// a durable engine surviving a crash, or left fresh to model one that lost all
/// state (so only the WAL can restore it).
#[derive(Clone, Default)]
struct MockEngine {
    map: Arc<Mutex<BTreeMap<Vec<u8>, Vec<u8>>>>,
}

impl MockEngine {
    fn snapshot(&self) -> BTreeMap<Vec<u8>, Vec<u8>> {
        self.map.lock().unwrap().clone()
    }
    fn from_snapshot(snap: BTreeMap<Vec<u8>, Vec<u8>>) -> Self {
        MockEngine {
            map: Arc::new(Mutex::new(snap)),
        }
    }
}

impl GeneralEngine for MockEngine {
    fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.map.lock().unwrap().get(key).cloned())
    }
    fn write(&self, batch: WriteBatch) -> Result<()> {
        let mut map = self.map.lock().unwrap();
        for op in batch.ops() {
            match op {
                WriteOp::Put(k, v) => {
                    map.insert(k.clone(), v.clone());
                }
                WriteOp::Delete(k) => {
                    map.remove(k);
                }
            }
        }
        Ok(())
    }
    fn scan_prefix<'a>(&'a self, prefix: &[u8]) -> Box<dyn Iterator<Item = ScanItem> + 'a> {
        let items: Vec<ScanItem> = self
            .map
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| Ok((k.clone(), v.clone())))
            .collect();
        Box::new(items.into_iter())
    }
    fn compact_range(&self, _range: Option<Range>) -> Result<()> {
        Ok(())
    }
    fn install_gc(&self, _predicate: Arc<dyn GcPredicate>) {}
}

fn put(batch_key: &str, val: &str) -> WriteBatch {
    let mut b = WriteBatch::new();
    b.put(batch_key.as_bytes().to_vec(), val.as_bytes().to_vec());
    b
}

const NEVER_CHECKPOINT: u64 = u64::MAX;

#[test]
fn write_logs_before_applying_and_is_visible() {
    let dir = TempDir::new().unwrap();
    let wal = Arc::new(FileWal::open(dir.path(), &WalConfig::default()).unwrap());
    let eng = WalEngine::recovered(MockEngine::default(), wal.clone(), NEVER_CHECKPOINT).unwrap();

    eng.write(put("a", "1")).unwrap();
    eng.write(put("b", "2")).unwrap();

    // Applied to the inner engine...
    assert_eq!(eng.get(b"a").unwrap(), Some(b"1".to_vec()));
    // ...and durably logged (two records on disk).
    assert_eq!(wal.replay(1).unwrap().len(), 2);
}

/// Crash where the engine loses everything: a fresh engine + the durable WAL
/// must reconstruct the full confirmed state (no loss).
#[test]
fn recovery_into_fresh_engine_restores_all_writes() {
    let dir = TempDir::new().unwrap();
    {
        let wal = Arc::new(FileWal::open(dir.path(), &WalConfig::default()).unwrap());
        let eng =
            WalEngine::recovered(MockEngine::default(), wal.clone(), NEVER_CHECKPOINT).unwrap();
        eng.write(put("k1", "v1")).unwrap();
        eng.write(put("k2", "v2")).unwrap();
        eng.write(put("k1", "v1b")).unwrap(); // overwrite
                                              // "crash": engine state (the default MockEngine) is dropped/lost.
    }
    let wal = Arc::new(FileWal::open(dir.path(), &WalConfig::default()).unwrap());
    let eng = WalEngine::recovered(MockEngine::default(), wal, NEVER_CHECKPOINT).unwrap();
    assert_eq!(eng.get(b"k1").unwrap(), Some(b"v1b".to_vec()));
    assert_eq!(eng.get(b"k2").unwrap(), Some(b"v2".to_vec()));
}

/// Replaying the same physical records more than once is idempotent: recovering
/// twice yields the identical engine state (no double-application).
#[test]
fn replay_is_idempotent() {
    let dir = TempDir::new().unwrap();
    {
        let wal = Arc::new(FileWal::open(dir.path(), &WalConfig::default()).unwrap());
        let eng = WalEngine::recovered(MockEngine::default(), wal, NEVER_CHECKPOINT).unwrap();
        eng.write(put("x", "10")).unwrap();
        eng.write(put("y", "20")).unwrap();
    }

    // First recovery.
    let once = {
        let wal = Arc::new(FileWal::open(dir.path(), &WalConfig::default()).unwrap());
        let eng = WalEngine::recovered(MockEngine::default(), wal, NEVER_CHECKPOINT).unwrap();
        eng.inner().snapshot()
    };
    // Recover the very same log again into a fresh engine.
    let twice = {
        let wal = Arc::new(FileWal::open(dir.path(), &WalConfig::default()).unwrap());
        let eng = WalEngine::recovered(MockEngine::default(), wal, NEVER_CHECKPOINT).unwrap();
        eng.inner().snapshot()
    };
    assert_eq!(once, twice, "replaying twice must reproduce the same state");

    // And replaying onto an engine that already has the data changes nothing.
    let wal = Arc::new(FileWal::open(dir.path(), &WalConfig::default()).unwrap());
    let prepopulated = MockEngine::from_snapshot(once.clone());
    let eng = WalEngine::recovered(prepopulated, wal, NEVER_CHECKPOINT).unwrap();
    assert_eq!(eng.inner().snapshot(), once);
}

/// With a durable engine, checkpointing bounds the retained log while a crash +
/// recovery still reconstructs the full state (post-checkpoint records replay
/// idempotently over the flushed engine).
#[test]
fn checkpoint_bounds_log_and_recovery_stays_complete() {
    let dir = TempDir::new().unwrap();
    let durable = MockEngine::default();
    let snap;
    {
        let wal = Arc::new(
            FileWal::open(
                dir.path(),
                &WalConfig {
                    segment_max_bytes: 128,
                },
            )
            .unwrap(),
        );
        // Small checkpoint threshold so checkpoints fire during the run.
        let eng = WalEngine::recovered(durable.clone(), wal.clone(), 256).unwrap();
        for i in 0..50 {
            eng.write(put(&format!("k{i:03}"), "some-value")).unwrap();
        }
        eng.checkpoint().unwrap();
        // Log is bounded well below the total bytes written.
        assert!(
            wal.pending_bytes() < 50 * 30,
            "log should be checkpointed down"
        );
        snap = durable.snapshot();
    }
    // "crash": durable engine keeps its (flushed) state; reopen WAL + recover.
    let wal = Arc::new(
        FileWal::open(
            dir.path(),
            &WalConfig {
                segment_max_bytes: 128,
            },
        )
        .unwrap(),
    );
    let eng = WalEngine::recovered(MockEngine::from_snapshot(snap), wal, 256).unwrap();
    for i in 0..50 {
        assert_eq!(
            eng.get(format!("k{i:03}").as_bytes()).unwrap(),
            Some(b"some-value".to_vec()),
            "key k{i:03} survived checkpoint + crash"
        );
    }
}
