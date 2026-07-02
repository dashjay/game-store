//! Per-command tests for the Set family: normal, boundary and error paths
//! (arity, WRONGTYPE, missing keys/members), plus the RESP2 vs RESP3
//! `SMEMBERS` reply shapes.

mod common;

use std::collections::BTreeSet;

use common::*;
use gamestore_protocol::{Frame, RespVersion};

/// Collect an array/set reply's bulk items for order-insensitive asserts.
fn members_of(frame: Frame) -> BTreeSet<Vec<u8>> {
    let items = match frame {
        Frame::Array(items) | Frame::Set(items) => items,
        other => panic!("expected array/set, got {other:?}"),
    };
    items
        .into_iter()
        .map(|f| match f {
            Frame::Bulk(b) => b.to_vec(),
            other => panic!("expected bulk member, got {other:?}"),
        })
        .collect()
}

#[test]
fn sadd_counts_only_new_members() {
    let db = TestDb::new();
    assert_int(db.exec(&["SADD", "s", "a", "b", "c"]), 3);
    // Existing members add nothing; new ones count.
    assert_int(db.exec(&["SADD", "s", "a", "d"]), 1);
    // Duplicates within one call count once.
    assert_int(db.exec(&["SADD", "s", "e", "e", "e"]), 1);
    assert_int(db.exec(&["SCARD", "s"]), 5);
    assert_simple(db.exec(&["TYPE", "s"]), "set");
    assert_wrong_args(db.exec(&["SADD", "s"]), "sadd");
    assert_wrong_args(db.exec(&["SADD"]), "sadd");
}

#[test]
fn srem_counts_removed_and_drops_empty_sets() {
    let db = TestDb::new();
    assert_int(db.exec(&["SADD", "s", "a", "b", "c"]), 3);
    assert_int(db.exec(&["SREM", "s", "a", "missing", "b"]), 2);
    assert_int(db.exec(&["SCARD", "s"]), 1);
    // Removing the last member deletes the key entirely.
    assert_int(db.exec(&["SREM", "s", "c"]), 1);
    assert_int(db.exec(&["EXISTS", "s"]), 0);
    assert_simple(db.exec(&["TYPE", "s"]), "none");
    // SREM on a missing key returns 0.
    assert_int(db.exec(&["SREM", "s", "a"]), 0);
    assert_wrong_args(db.exec(&["SREM", "s"]), "srem");
}

#[test]
fn sismember_yes_no_and_missing_key() {
    let db = TestDb::new();
    assert_int(db.exec(&["SADD", "s", "a"]), 1);
    assert_int(db.exec(&["SISMEMBER", "s", "a"]), 1);
    assert_int(db.exec(&["SISMEMBER", "s", "b"]), 0);
    assert_int(db.exec(&["SISMEMBER", "missing", "a"]), 0);
    assert_wrong_args(db.exec(&["SISMEMBER", "s"]), "sismember");
    assert_wrong_args(db.exec(&["SISMEMBER", "s", "a", "b"]), "sismember");
}

#[test]
fn smembers_resp2_array_resp3_set() {
    let db = TestDb::new();
    assert_int(db.exec(&["SADD", "s", "a", "b"]), 2);

    let v2 = db.exec(&["SMEMBERS", "s"]);
    assert!(matches!(v2, Frame::Array(_)), "RESP2 shape: {v2:?}");
    assert_eq!(
        members_of(v2),
        BTreeSet::from([b"a".to_vec(), b"b".to_vec()])
    );

    let v3 = db.exec_v(RespVersion::V3, &["SMEMBERS", "s"]);
    assert!(matches!(v3, Frame::Set(_)), "RESP3 shape: {v3:?}");
    assert_eq!(
        members_of(v3),
        BTreeSet::from([b"a".to_vec(), b"b".to_vec()])
    );

    // Missing key -> empty collection, not an error.
    assert_eq!(db.exec(&["SMEMBERS", "nope"]), Frame::Array(vec![]));
    assert_eq!(
        db.exec_v(RespVersion::V3, &["SMEMBERS", "nope"]),
        Frame::Set(vec![])
    );
    assert_wrong_args(db.exec(&["SMEMBERS"]), "smembers");
}

#[test]
fn scard_normal_and_missing() {
    let db = TestDb::new();
    assert_int(db.exec(&["SCARD", "missing"]), 0);
    assert_int(db.exec(&["SADD", "s", "a", "b"]), 2);
    assert_int(db.exec(&["SCARD", "s"]), 2);
    assert_wrong_args(db.exec(&["SCARD"]), "scard");
    assert_wrong_args(db.exec(&["SCARD", "s", "extra"]), "scard");
}

#[test]
fn set_commands_against_other_types_are_wrongtype() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "str", "v"]));
    for cmd in [
        vec!["SADD", "str", "m"],
        vec!["SREM", "str", "m"],
        vec!["SISMEMBER", "str", "m"],
        vec!["SMEMBERS", "str"],
        vec!["SCARD", "str"],
    ] {
        assert_wrong_type(db.exec(&cmd));
    }
    // And the reverse: other type families reject a Set key.
    assert_int(db.exec(&["SADD", "s", "m"]), 1);
    assert_wrong_type(db.exec(&["GET", "s"]));
    assert_wrong_type(db.exec(&["HGET", "s", "f"]));
    assert_wrong_type(db.exec(&["LPUSH", "s", "v"]));
    assert_wrong_type(db.exec(&["ZADD", "s", "1", "m"]));
}

#[test]
fn generic_commands_apply_to_sets() {
    let db = TestDb::new();
    assert_int(db.exec(&["SADD", "s", "a"]), 1);
    assert_int(db.exec(&["EXISTS", "s"]), 1);
    assert_int(db.exec(&["EXPIRE", "s", "100"]), 1);
    let ttl = int_of(db.exec(&["TTL", "s"]));
    assert!(ttl > 0 && ttl <= 100, "ttl={ttl}");
    assert_int(db.exec(&["DEL", "s"]), 1);
    assert_int(db.exec(&["EXISTS", "s"]), 0);
}
