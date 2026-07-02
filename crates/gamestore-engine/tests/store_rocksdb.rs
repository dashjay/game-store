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

/// `FLUSHDB`/`FLUSHALL` (I-05): every metadata + subkey record is removed and
/// the version map is cleared, so the store behaves like a fresh database.
#[test]
fn flush_all_clears_everything() {
    let dir = TempDir::new().unwrap();
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
    assert_eq!(s.dbsize().unwrap(), 2);
    assert_eq!(s.raw_subkey_count().unwrap(), 2);

    s.flush_all().unwrap();
    assert_eq!(s.dbsize().unwrap(), 0);
    assert_eq!(s.raw_subkey_count().unwrap(), 0);
    assert_eq!(s.get(b"str").unwrap(), None);
    assert_eq!(s.hlen(b"h").unwrap(), 0);
    assert_eq!(s.type_of(b"h").unwrap(), "none");

    // The version table was cleared too: a recreated hash starts fresh and a
    // compaction right after flush has nothing stale to reclaim.
    s.hset(b"h", &[(b"new".to_vec(), b"1".to_vec())]).unwrap();
    assert_eq!(
        s.hgetall(b"h").unwrap(),
        vec![(b"new".to_vec(), b"1".to_vec())]
    );
    s.compact().unwrap();
    assert_eq!(s.raw_subkey_count().unwrap(), 1);
}

/// Flush must also survive a restart: nothing reappears after reopening.
#[test]
fn flush_all_survives_restart() {
    let dir = TempDir::new().unwrap();

    {
        let s = open(&dir);
        s.hset(b"h", &[(b"a".to_vec(), b"1".to_vec())]).unwrap();
        s.set(b"k", b"v", 0).unwrap();
        s.flush_all().unwrap();
    }

    {
        let s = open(&dir);
        assert_eq!(s.dbsize().unwrap(), 0);
        assert_eq!(s.raw_subkey_count().unwrap(), 0);
        assert_eq!(s.get(b"k").unwrap(), None);
        assert_eq!(s.hlen(b"h").unwrap(), 0);
    }
}

// ---- Set (I-06) --------------------------------------------------------------

#[test]
fn set_add_rem_members() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    assert_eq!(
        s.sadd(b"s", &[b"a".to_vec(), b"b".to_vec(), b"a".to_vec()])
            .unwrap(),
        2,
        "duplicate within one call counts once"
    );
    assert_eq!(s.type_of(b"s").unwrap(), "set");
    assert_eq!(s.scard(b"s").unwrap(), 2);
    assert!(s.sismember(b"s", b"a").unwrap());
    assert!(!s.sismember(b"s", b"missing").unwrap());
    // Re-adding an existing member adds nothing.
    assert_eq!(s.sadd(b"s", &[b"a".to_vec()]).unwrap(), 0);

    let mut members = s.smembers(b"s").unwrap();
    members.sort();
    assert_eq!(members, vec![b"a".to_vec(), b"b".to_vec()]);

    assert_eq!(
        s.srem(b"s", &[b"a".to_vec(), b"missing".to_vec()]).unwrap(),
        1
    );
    assert_eq!(s.scard(b"s").unwrap(), 1);
    // Removing the last member deletes the key.
    assert_eq!(s.srem(b"s", &[b"b".to_vec()]).unwrap(), 1);
    assert!(!s.exists(b"s").unwrap());
    assert_eq!(s.type_of(b"s").unwrap(), "none");
    // Missing-key reads are well-defined.
    assert_eq!(s.smembers(b"s").unwrap(), Vec::<Vec<u8>>::new());
    assert_eq!(s.scard(b"s").unwrap(), 0);
    assert_eq!(s.srem(b"s", &[b"a".to_vec()]).unwrap(), 0);
}

// ---- ZSet (I-06) -------------------------------------------------------------

#[test]
fn zset_add_score_rem() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    assert_eq!(
        s.zadd(b"z", &[(1.0, b"a".to_vec()), (2.0, b"b".to_vec())])
            .unwrap(),
        2
    );
    assert_eq!(s.type_of(b"z").unwrap(), "zset");
    assert_eq!(s.zcard(b"z").unwrap(), 2);
    assert_eq!(s.zscore(b"z", b"a").unwrap(), Some(1.0));
    assert_eq!(s.zscore(b"z", b"missing").unwrap(), None);

    // Updating an existing member's score adds nothing but moves its rank.
    assert_eq!(s.zadd(b"z", &[(3.0, b"a".to_vec())]).unwrap(), 0);
    assert_eq!(s.zscore(b"z", b"a").unwrap(), Some(3.0));
    assert_eq!(
        s.zrange(b"z", 0, -1).unwrap(),
        vec![(b"b".to_vec(), 2.0), (b"a".to_vec(), 3.0)]
    );

    // Duplicate member within one call: last score wins, counted once.
    assert_eq!(
        s.zadd(b"z2", &[(1.0, b"m".to_vec()), (9.0, b"m".to_vec())])
            .unwrap(),
        1
    );
    assert_eq!(s.zscore(b"z2", b"m").unwrap(), Some(9.0));

    assert_eq!(s.zrem(b"z", &[b"a".to_vec(), b"nope".to_vec()]).unwrap(), 1);
    assert_eq!(s.zcard(b"z").unwrap(), 1);
    // Removing the last member deletes the key.
    assert_eq!(s.zrem(b"z", &[b"b".to_vec()]).unwrap(), 1);
    assert!(!s.exists(b"z").unwrap());
    assert_eq!(s.zcard(b"z").unwrap(), 0);
}

#[test]
fn zset_range_orders_by_score_then_member() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    s.zadd(
        b"lb",
        &[
            (10.0, b"carol".to_vec()),
            (-5.0, b"alice".to_vec()),
            (10.0, b"bob".to_vec()),
            (0.0, b"dave".to_vec()),
        ],
    )
    .unwrap();

    // Ascending (score, member) order; ties broken lexicographically.
    assert_eq!(
        s.zrange(b"lb", 0, -1).unwrap(),
        vec![
            (b"alice".to_vec(), -5.0),
            (b"dave".to_vec(), 0.0),
            (b"bob".to_vec(), 10.0),
            (b"carol".to_vec(), 10.0),
        ]
    );
    // Rank sub-ranges and negative indexes.
    assert_eq!(
        s.zrange(b"lb", 1, 2).unwrap(),
        vec![(b"dave".to_vec(), 0.0), (b"bob".to_vec(), 10.0)]
    );
    assert_eq!(
        s.zrange(b"lb", -2, -1).unwrap(),
        vec![(b"bob".to_vec(), 10.0), (b"carol".to_vec(), 10.0)]
    );
    // Out-of-range / inverted ranges are empty.
    assert_eq!(s.zrange(b"lb", 5, 9).unwrap(), vec![]);
    assert_eq!(s.zrange(b"lb", 2, 1).unwrap(), vec![]);

    // Score ranges: inclusive, exclusive and infinite bounds.
    assert_eq!(
        s.zrange_by_score(b"lb", 0.0, false, 10.0, false).unwrap(),
        vec![
            (b"dave".to_vec(), 0.0),
            (b"bob".to_vec(), 10.0),
            (b"carol".to_vec(), 10.0),
        ]
    );
    assert_eq!(
        s.zrange_by_score(b"lb", 0.0, true, 10.0, true).unwrap(),
        vec![]
    );
    assert_eq!(
        s.zrange_by_score(b"lb", f64::NEG_INFINITY, false, 0.0, true)
            .unwrap(),
        vec![(b"alice".to_vec(), -5.0)]
    );
    assert_eq!(
        s.zrange_by_score(b"lb", f64::NEG_INFINITY, false, f64::INFINITY, false)
            .unwrap()
            .len(),
        4
    );
}

// ---- List (I-06) -------------------------------------------------------------

#[test]
fn list_push_pop_range() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    // RPUSH a b, LPUSH x y -> y x a b.
    assert_eq!(
        s.push(b"l", &[b"a".to_vec(), b"b".to_vec()], false)
            .unwrap(),
        2
    );
    assert_eq!(
        s.push(b"l", &[b"x".to_vec(), b"y".to_vec()], true).unwrap(),
        4
    );
    assert_eq!(s.type_of(b"l").unwrap(), "list");
    assert_eq!(s.llen(b"l").unwrap(), 4);
    assert_eq!(
        s.lrange(b"l", 0, -1).unwrap(),
        vec![b"y".to_vec(), b"x".to_vec(), b"a".to_vec(), b"b".to_vec()]
    );
    // Sub-ranges, negative indexes, clamping.
    assert_eq!(
        s.lrange(b"l", 1, 2).unwrap(),
        vec![b"x".to_vec(), b"a".to_vec()]
    );
    assert_eq!(
        s.lrange(b"l", -2, 99).unwrap(),
        vec![b"a".to_vec(), b"b".to_vec()]
    );
    assert_eq!(s.lrange(b"l", 3, 1).unwrap(), Vec::<Vec<u8>>::new());

    assert_eq!(s.pop(b"l", 1, true).unwrap(), vec![b"y".to_vec()]);
    assert_eq!(s.pop(b"l", 1, false).unwrap(), vec![b"b".to_vec()]);
    assert_eq!(s.llen(b"l").unwrap(), 2);
    // Multi-pop drains in order and deletes the emptied key.
    assert_eq!(
        s.pop(b"l", 10, true).unwrap(),
        vec![b"x".to_vec(), b"a".to_vec()]
    );
    assert!(!s.exists(b"l").unwrap());
    assert_eq!(s.llen(b"l").unwrap(), 0);
    assert_eq!(s.pop(b"l", 1, true).unwrap(), Vec::<Vec<u8>>::new());
    assert_eq!(s.lrange(b"l", 0, -1).unwrap(), Vec::<Vec<u8>>::new());
}

// ---- cross-type behavior (I-06) ------------------------------------------------

#[test]
fn wrong_type_errors_cover_new_types() {
    use gamestore_engine::EngineError;
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    s.set(b"str", b"v", 0).unwrap();
    assert!(matches!(
        s.sadd(b"str", &[b"m".to_vec()]),
        Err(EngineError::WrongType)
    ));
    assert!(matches!(s.smembers(b"str"), Err(EngineError::WrongType)));
    assert!(matches!(
        s.zadd(b"str", &[(1.0, b"m".to_vec())]),
        Err(EngineError::WrongType)
    ));
    assert!(matches!(
        s.zrange(b"str", 0, -1),
        Err(EngineError::WrongType)
    ));
    assert!(matches!(
        s.push(b"str", &[b"v".to_vec()], true),
        Err(EngineError::WrongType)
    ));
    assert!(matches!(
        s.lrange(b"str", 0, -1),
        Err(EngineError::WrongType)
    ));

    // Every composite type rejects operations of the other composite types.
    s.sadd(b"set", &[b"m".to_vec()]).unwrap();
    s.zadd(b"zset", &[(1.0, b"m".to_vec())]).unwrap();
    s.push(b"list", &[b"v".to_vec()], false).unwrap();
    assert!(matches!(s.hget(b"set", b"f"), Err(EngineError::WrongType)));
    assert!(matches!(
        s.zscore(b"set", b"m"),
        Err(EngineError::WrongType)
    ));
    assert!(matches!(
        s.sismember(b"zset", b"m"),
        Err(EngineError::WrongType)
    ));
    assert!(matches!(s.llen(b"zset"), Err(EngineError::WrongType)));
    assert!(matches!(
        s.pop(b"set", 1, true),
        Err(EngineError::WrongType)
    ));
    assert!(matches!(s.scard(b"list"), Err(EngineError::WrongType)));
    assert!(matches!(s.get(b"list"), Err(EngineError::WrongType)));

    // SET always overwrites, regardless of prior type.
    s.set(b"zset", b"now-a-string", 0).unwrap();
    assert_eq!(s.get(b"zset").unwrap(), Some(b"now-a-string".to_vec()));
}

/// DEL is an O(1) version bump for the new types too, and the compaction
/// filter physically reclaims their per-member records (including the ZSet
/// score index, which doubles the physical record count).
#[test]
fn compaction_gc_reclaims_new_type_records() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    let members: Vec<Vec<u8>> = (0..50).map(|i| format!("m{i}").into_bytes()).collect();
    s.sadd(b"set", &members).unwrap();
    let scored: Vec<(f64, Vec<u8>)> = (0..50)
        .map(|i| (i as f64, format!("m{i}").into_bytes()))
        .collect();
    s.zadd(b"zset", &scored).unwrap();
    s.push(b"list", &members, false).unwrap();

    // set 50 + zset 50*2 (member + score index) + list 50.
    assert_eq!(s.raw_subkey_count().unwrap(), 200);

    assert!(s.del(b"set").unwrap());
    assert!(s.del(b"zset").unwrap());
    assert!(s.del(b"list").unwrap());
    assert_eq!(s.dbsize().unwrap(), 0);
    s.compact().unwrap();
    assert_eq!(s.raw_subkey_count().unwrap(), 0);
}

/// A ZSet score update must leave no stale score-index record behind after
/// compaction (the old (score, member) entry is deleted in the same batch).
#[test]
fn zset_score_update_leaves_no_stale_index() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    s.zadd(b"z", &[(1.0, b"m".to_vec())]).unwrap();
    s.zadd(b"z", &[(2.0, b"m".to_vec())]).unwrap();
    // One member subkey + one score-index record, no leftovers.
    assert_eq!(s.raw_subkey_count().unwrap(), 2);
    assert_eq!(s.zrange(b"z", 0, -1).unwrap(), vec![(b"m".to_vec(), 2.0)]);
}

/// TTL applies to the new types via the shared metadata: an expired composite
/// key reads as missing and its records become GC-able garbage.
#[test]
fn expiry_applies_to_new_types() {
    let dir = TempDir::new().unwrap();
    let s = open(&dir);

    s.sadd(b"s", &[b"m".to_vec()]).unwrap();
    s.zadd(b"z", &[(1.0, b"m".to_vec())]).unwrap();
    s.push(b"l", &[b"v".to_vec()], false).unwrap();

    // Absolute deadline of 1ms-after-epoch: long past -> lazily expired.
    for key in [b"s".as_slice(), b"z", b"l"] {
        assert!(s.expire_at(key, 1).unwrap());
        assert!(!s.exists(key).unwrap());
        assert_eq!(s.type_of(key).unwrap(), "none");
        assert_eq!(s.pttl(key).unwrap(), -2);
    }
    assert_eq!(s.scard(b"s").unwrap(), 0);
    assert_eq!(s.zcard(b"z").unwrap(), 0);
    assert_eq!(s.llen(b"l").unwrap(), 0);

    // The lazy delete dropped the version-map entries -> records reclaimable.
    s.compact().unwrap();
    assert_eq!(s.raw_subkey_count().unwrap(), 0);
}

/// Restart: version maps for the new types are rebuilt from metadata so reads
/// resolve correctly and GC still works after reopening.
#[test]
fn new_types_survive_restart() {
    let dir = TempDir::new().unwrap();

    {
        let s = open(&dir);
        s.sadd(b"s", &[b"a".to_vec(), b"b".to_vec()]).unwrap();
        s.zadd(b"z", &[(1.5, b"m1".to_vec()), (-2.0, b"m2".to_vec())])
            .unwrap();
        s.push(b"l", &[b"one".to_vec(), b"two".to_vec()], false)
            .unwrap();
    } // drop -> close DB

    {
        let s = open(&dir);
        let mut members = s.smembers(b"s").unwrap();
        members.sort();
        assert_eq!(members, vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(s.scard(b"s").unwrap(), 2);

        assert_eq!(s.zscore(b"z", b"m1").unwrap(), Some(1.5));
        assert_eq!(
            s.zrange(b"z", 0, -1).unwrap(),
            vec![(b"m2".to_vec(), -2.0), (b"m1".to_vec(), 1.5)]
        );

        assert_eq!(
            s.lrange(b"l", 0, -1).unwrap(),
            vec![b"one".to_vec(), b"two".to_vec()]
        );
        // List bounds survive: pushes keep extending correctly.
        assert_eq!(s.push(b"l", &[b"zero".to_vec()], true).unwrap(), 3);
        assert_eq!(s.pop(b"l", 1, false).unwrap(), vec![b"two".to_vec()]);

        // GC still works after restart for the rebuilt versions.
        s.del(b"s").unwrap();
        s.del(b"z").unwrap();
        s.del(b"l").unwrap();
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
