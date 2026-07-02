//! Integration tests for the RocksDB-backed [`Store`] (plan I-03 DoD):
//!
//! - GC: stale versions / orphan subkeys are physically reclaimed to 0 after a
//!   forced compaction;
//! - restart: the in-memory version map is rebuilt from persisted metadata and
//!   only the newest version's data is visible;
//! - String / Hash operations and Redis-style `WRONGTYPE` behavior.

use gamestore_engine::{EngineConfig, Store};
use tempfile::TempDir;

fn open(dir: &TempDir) -> Store<gamestore_engine::RocksEngine> {
    Store::open(dir.path(), &EngineConfig::default()).expect("open store")
}

#[test]
fn string_set_get_del_ttl() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    assert_eq!(s.get(b"k").unwrap(), None);
    s.set(b"k", b"v", 0).unwrap();
    assert_eq!(s.get(b"k").unwrap(), Some(b"v".to_vec()));
    assert!(s.exists(b"k").unwrap());
    assert_eq!(s.type_of(b"k").unwrap(), "string");
    assert_eq!(s.pttl(b"k").unwrap(), -1); // exists, no expiry

    assert!(s.del(b"k").unwrap());
    assert!(!s.del(b"k").unwrap());
    assert_eq!(s.get(b"k").unwrap(), None);
    assert_eq!(s.pttl(b"k").unwrap(), -2); // missing
    assert_eq!(s.type_of(b"k").unwrap(), "none");
}

#[test]
fn lazy_expiration() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    // Already-expired absolute timestamp -> treated as missing on access.
    s.set(b"k", b"v", 1).unwrap();
    assert_eq!(s.get(b"k").unwrap(), None);
    assert!(!s.exists(b"k").unwrap());

    // Future expiry is live and reports a positive TTL.
    let future = gamestore_engine::now_ms() + 100_000;
    s.set(b"k2", b"v", future).unwrap();
    assert!(s.exists(b"k2").unwrap());
    let ttl = s.pttl(b"k2").unwrap();
    assert!(ttl > 0 && ttl <= 100_000, "ttl={ttl}");

    // expire_at on a missing key returns false.
    assert!(!s.expire_at(b"missing", future).unwrap());
    assert!(s.expire_at(b"k2", future).unwrap());
}

#[test]
fn hash_ops() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    let created = s
        .hset(
            b"h",
            &[
                (b"a".to_vec(), b"1".to_vec()),
                (b"b".to_vec(), b"2".to_vec()),
            ],
        )
        .unwrap();
    assert_eq!(created, 2);
    assert_eq!(s.type_of(b"h").unwrap(), "hash");
    assert_eq!(s.hlen(b"h").unwrap(), 2);
    assert_eq!(s.hget(b"h", b"a").unwrap(), Some(b"1".to_vec()));
    assert!(s.hexists(b"h", b"b").unwrap());
    assert!(!s.hexists(b"h", b"c").unwrap());

    // Overwriting an existing field creates 0 new fields.
    let created = s.hset(b"h", &[(b"a".to_vec(), b"9".to_vec())]).unwrap();
    assert_eq!(created, 0);
    assert_eq!(s.hget(b"h", b"a").unwrap(), Some(b"9".to_vec()));

    let mut all = s.hgetall(b"h").unwrap();
    all.sort();
    assert_eq!(
        all,
        vec![
            (b"a".to_vec(), b"9".to_vec()),
            (b"b".to_vec(), b"2".to_vec())
        ]
    );

    assert_eq!(
        s.hdel(b"h", &[b"a".to_vec(), b"missing".to_vec()]).unwrap(),
        1
    );
    assert_eq!(s.hlen(b"h").unwrap(), 1);

    // Deleting the last field removes the whole key.
    assert_eq!(s.hdel(b"h", &[b"b".to_vec()]).unwrap(), 1);
    assert_eq!(s.type_of(b"h").unwrap(), "none");
    assert!(!s.exists(b"h").unwrap());
}

#[test]
fn wrong_type_errors() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    s.set(b"str", b"v", 0).unwrap();
    assert!(matches!(
        s.hget(b"str", b"f"),
        Err(gamestore_engine::EngineError::WrongType)
    ));
    assert!(matches!(
        s.hset(b"str", &[(b"f".to_vec(), b"v".to_vec())]),
        Err(gamestore_engine::EngineError::WrongType)
    ));

    s.hset(b"hash", &[(b"f".to_vec(), b"v".to_vec())]).unwrap();
    assert!(matches!(
        s.get(b"hash"),
        Err(gamestore_engine::EngineError::WrongType)
    ));

    // SET always overwrites, regardless of prior type.
    s.set(b"hash", b"now-a-string", 0).unwrap();
    assert_eq!(s.get(b"hash").unwrap(), Some(b"now-a-string".to_vec()));
}

/// Core DoD: delete → O(1) version bump → orphan subkeys reclaimed to 0 by the
/// compaction filter; a rebuilt (new-version) hash only shows the new data.
#[test]
fn compaction_gc_reclaims_stale_subkeys() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    // 100 fields under version v1.
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = (0..100)
        .map(|i| (format!("f{i}").into_bytes(), format!("v{i}").into_bytes()))
        .collect();
    s.hset(b"h", &pairs).unwrap();
    assert_eq!(s.raw_subkey_count().unwrap(), 100);
    assert_eq!(s.dbsize().unwrap(), 1);

    // DEL bumps the logical delete; the 100 subkeys become orphans.
    assert!(s.del(b"h").unwrap());
    assert_eq!(s.dbsize().unwrap(), 0);

    // Before compaction the orphans are still physically present...
    assert_eq!(s.raw_subkey_count().unwrap(), 100);
    // ...after a forced compaction the filter reclaims them to zero.
    s.compact().unwrap();
    assert_eq!(s.raw_subkey_count().unwrap(), 0);

    // Rebuild the key: fresh version, new fields only.
    s.hset(b"h", &[(b"only".to_vec(), b"new".to_vec())])
        .unwrap();
    s.compact().unwrap();
    assert_eq!(s.raw_subkey_count().unwrap(), 1);
    assert_eq!(s.hget(b"h", b"only").unwrap(), Some(b"new".to_vec()));
    assert_eq!(s.hget(b"h", b"f0").unwrap(), None);
}

/// Overwriting a hash with more `HSET`s at a *new* version (via delete+recreate)
/// leaves stale-version subkeys that GC must drop while keeping current ones.
#[test]
fn gc_keeps_current_version_after_rebuild() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    s.hset(b"h", &[(b"a".to_vec(), b"1".to_vec())]).unwrap();
    s.del(b"h").unwrap(); // orphan the v1 subkey
    s.hset(
        b"h",
        &[
            (b"a".to_vec(), b"2".to_vec()),
            (b"b".to_vec(), b"3".to_vec()),
        ],
    )
    .unwrap(); // v2 subkeys

    // Two live subkeys + one orphan physically present pre-compaction.
    assert_eq!(s.raw_subkey_count().unwrap(), 3);
    s.compact().unwrap();
    assert_eq!(s.raw_subkey_count().unwrap(), 2);
    assert_eq!(s.hget(b"h", b"a").unwrap(), Some(b"2".to_vec()));
    assert_eq!(s.hget(b"h", b"b").unwrap(), Some(b"3".to_vec()));
}

/// Core DoD: after reopening, the version map is rebuilt from metadata so hash
/// reads resolve to the correct (current) version.
#[test]
fn version_map_rebuilt_on_restart() {
    let dir = TempDir::new().unwrap();

    {
        let s = open(&dir);
        s.set(b"str", b"v", 0).unwrap();
        s.hset(
            b"h",
            &[
                (b"a".to_vec(), b"1".to_vec()),
                (b"b".to_vec(), b"2".to_vec()),
            ],
        )
        .unwrap();
    } // drop -> close DB

    {
        let s = open(&dir);
        // String survives.
        assert_eq!(s.get(b"str").unwrap(), Some(b"v".to_vec()));
        // Hash resolves via the rebuilt version map.
        assert_eq!(s.hlen(b"h").unwrap(), 2);
        assert_eq!(s.hget(b"h", b"a").unwrap(), Some(b"1".to_vec()));
        let mut all = s.hgetall(b"h").unwrap();
        all.sort();
        assert_eq!(
            all,
            vec![
                (b"a".to_vec(), b"1".to_vec()),
                (b"b".to_vec(), b"2".to_vec())
            ]
        );

        // GC still works after restart: delete + compact reclaims subkeys.
        s.del(b"h").unwrap();
        s.compact().unwrap();
        assert_eq!(s.raw_subkey_count().unwrap(), 0);
    }
}

/// After a restart, a stale-version subkey left on disk must be GC'd and never
/// leak into reads of the rebuilt key.
#[test]
fn restart_then_gc_drops_stale_version() {
    let dir = TempDir::new().unwrap();

    {
        let s = open(&dir);
        s.hset(b"h", &[(b"old".to_vec(), b"1".to_vec())]).unwrap();
        s.del(b"h").unwrap(); // v1 subkey now orphaned, still on disk (no compact)
        assert_eq!(s.raw_subkey_count().unwrap(), 1);
    }

    {
        let s = open(&dir);
        // Recreate at a new version.
        s.hset(b"h", &[(b"new".to_vec(), b"2".to_vec())]).unwrap();
        s.compact().unwrap();
        // Only the current-version subkey remains.
        assert_eq!(s.raw_subkey_count().unwrap(), 1);
        assert_eq!(s.hget(b"h", b"new").unwrap(), Some(b"2".to_vec()));
        assert_eq!(s.hget(b"h", b"old").unwrap(), None);
    }
}
