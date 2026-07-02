//! Rust port of `spike/test/redis_functional_test.py`: the same 32 assertions,
//! in the same order, driven through the command layer instead of a socket
//! (I-04 DoD — the true end-to-end run over TCP lands with the I-05 server
//! assembly). Each `flushdb()` in the Python script maps to a fresh store.

mod common;

use std::collections::HashMap;

use common::*;
use gamestore_protocol::Frame;

fn hgetall_as_map(db: &TestDb, key: &str) -> HashMap<Vec<u8>, Vec<u8>> {
    let Frame::Array(items) = db.exec(&["HGETALL", key]) else {
        panic!("expected flat array");
    };
    items
        .chunks_exact(2)
        .map(|kv| match (&kv[0], &kv[1]) {
            (Frame::Bulk(f), Frame::Bulk(v)) => (f.to_vec(), v.to_vec()),
            other => panic!("expected bulk pair, got {other:?}"),
        })
        .collect()
}

#[test]
fn spike_functional_suite_connectivity_string_ttl_hash() {
    let db = TestDb::new();

    // --- connectivity ---
    assert_simple(db.exec(&["PING"]), "PONG"); // PING
    assert_bulk(db.exec(&["ECHO", "hi"]), "hi"); // ECHO

    // --- String ---
    assert_ok(db.exec(&["SET", "k1", "v1"])); // SET
    assert_bulk(db.exec(&["GET", "k1"]), "v1"); // GET
    assert_null(db.exec(&["GET", "nope"])); // GET missing
    assert_simple(db.exec(&["TYPE", "k1"]), "string"); // TYPE string
    assert_int(db.exec(&["EXISTS", "k1"]), 1); // EXISTS
    assert_int(db.exec(&["DEL", "k1"]), 1); // DEL
    assert_null(db.exec(&["GET", "k1"])); // GET after DEL
    assert_int(db.exec(&["EXISTS", "k1"]), 0); // EXISTS after DEL

    // --- String overwrite + TTL ---
    assert_ok(db.exec(&["SET", "t1", "x", "PX", "150"]));
    let pttl = int_of(db.exec(&["PTTL", "t1"]));
    assert!(0 < pttl && pttl <= 150, "pttl={pttl}"); // PTTL set
    std::thread::sleep(std::time::Duration::from_millis(250));
    assert_null(db.exec(&["GET", "t1"])); // GET after expiry
    assert_int(db.exec(&["TTL", "missing"]), -2); // TTL missing key
    assert_ok(db.exec(&["SET", "t2", "y"]));
    assert_int(db.exec(&["TTL", "t2"]), -1); // TTL no expiry

    // --- Hash (player data, the main carrier) ---
    let player = "player:{1001}";
    assert_int(
        db.exec(&["HSET", player, "gold", "100", "level", "5", "hp", "42"]),
        3,
    ); // HSET new fields
    assert_bulk(db.exec(&["HGET", player, "gold"]), "100"); // HGET
    assert_int(db.exec(&["HLEN", player]), 3); // HLEN
    assert_int(db.exec(&["HEXISTS", player, "hp"]), 1); // HEXISTS yes
    assert_int(db.exec(&["HEXISTS", player, "mana"]), 0); // HEXISTS no
    assert_eq!(
        db.exec(&["HMGET", player, "gold", "level", "missing"]),
        Frame::Array(vec![
            Frame::Bulk("100".into()),
            Frame::Bulk("5".into()),
            Frame::Null,
        ])
    ); // HMGET
    let all = hgetall_as_map(&db, player);
    let want: HashMap<Vec<u8>, Vec<u8>> = [
        (b"gold".to_vec(), b"100".to_vec()),
        (b"level".to_vec(), b"5".to_vec()),
        (b"hp".to_vec(), b"42".to_vec()),
    ]
    .into_iter()
    .collect();
    assert_eq!(all, want); // HGETALL
    assert_simple(db.exec(&["TYPE", player]), "hash"); // TYPE hash
    assert_int(db.exec(&["HSET", player, "gold", "200"]), 0); // HSET update existing
    assert_bulk(db.exec(&["HGET", player, "gold"]), "200"); // HGET updated
    assert_int(db.exec(&["HDEL", player, "hp"]), 1); // HDEL
    assert_int(db.exec(&["HLEN", player]), 2); // HLEN after HDEL
}

#[test]
fn spike_functional_suite_version_gc_via_compaction_filter() {
    // Fresh DB (the Python script calls flushdb here) so RAWCOUNT reflects
    // only this hash's subkeys.
    let db = TestDb::new();
    let big = "player:{gc}";
    let mut args: Vec<String> = vec!["HSET".into(), big.into()];
    for i in 0..200 {
        args.push(format!("f{i}"));
        args.push(i.to_string());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    assert_int(db.exec(&arg_refs), 200);

    let raw_before = int_of(db.exec(&["RAWCOUNT"]));
    assert!(raw_before >= 200, "raw_before={raw_before}"); // RAWCOUNT has subkeys before DEL
    assert_int(db.exec(&["DEL", big]), 1); // DEL big hash (O(1) version bump)
    assert_ok(db.exec(&["COMPACT"]));
    assert_int(db.exec(&["RAWCOUNT"]), 0); // subkeys reclaimed by compaction filter

    // --- Rebuild after delete uses a fresh version (no stale leakage) ---
    assert_int(db.exec(&["HSET", big, "a", "1", "b", "2"]), 2);
    assert_int(db.exec(&["DEL", big]), 1);
    assert_int(db.exec(&["HSET", big, "only", "new"]), 1); // recreated; old version orphaned
    assert_ok(db.exec(&["COMPACT"]));
    let all = hgetall_as_map(&db, big);
    let want: HashMap<Vec<u8>, Vec<u8>> =
        [(b"only".to_vec(), b"new".to_vec())].into_iter().collect();
    assert_eq!(all, want); // recreated hash sees only new fields
    assert_int(db.exec(&["HLEN", big]), 1); // recreated hash HLEN
}

#[test]
fn spike_functional_suite_dbsize_on_fresh_db() {
    // The Python suite ends with `flushdb; DBSIZE == 0`; FLUSHDB itself is an
    // I-05 admin verb, so the equivalent here is a fresh store.
    let db = TestDb::new();
    assert_int(db.exec(&["DBSIZE"]), 0); // DBSIZE after flush
}
