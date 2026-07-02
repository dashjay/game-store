//! Per-command tests for the Hash family: normal, boundary and error paths
//! (arity incl. odd field/value tails, WRONGTYPE, missing keys/fields), plus
//! the RESP2 vs RESP3 `HGETALL` reply shapes.

mod common;

use std::collections::HashMap;

use bytes::Bytes;
use common::*;
use gamestore_protocol::{Frame, RespVersion};

/// Collect a RESP2 flat HGETALL array into a map for order-insensitive asserts.
fn flat_to_map(frame: Frame) -> HashMap<Vec<u8>, Vec<u8>> {
    let Frame::Array(items) = frame else {
        panic!("expected flat array, got {frame:?}");
    };
    assert!(items.len() % 2 == 0, "odd flat array: {items:?}");
    items
        .chunks_exact(2)
        .map(|kv| match (&kv[0], &kv[1]) {
            (Frame::Bulk(f), Frame::Bulk(v)) => (f.to_vec(), v.to_vec()),
            other => panic!("expected bulk pair, got {other:?}"),
        })
        .collect()
}

// ---- HSET / HMSET -------------------------------------------------------------

#[test]
fn hset_returns_created_count_and_updates_in_place() {
    let db = TestDb::new();
    assert_int(
        db.exec(&["HSET", "h", "gold", "100", "level", "5", "hp", "42"]),
        3,
    );
    // Updating an existing field creates nothing new.
    assert_int(db.exec(&["HSET", "h", "gold", "200"]), 0);
    assert_bulk(db.exec(&["HGET", "h", "gold"]), "200");
    // Mixed new + existing counts only the new field.
    assert_int(db.exec(&["HSET", "h", "gold", "300", "mana", "7"]), 1);
    assert_int(db.exec(&["HLEN", "h"]), 4);
}

#[test]
fn hmset_replies_ok_instead_of_a_count() {
    let db = TestDb::new();
    assert_ok(db.exec(&["HMSET", "h", "a", "1", "b", "2"]));
    assert_bulk(db.exec(&["HGET", "h", "a"]), "1");
    assert_int(db.exec(&["HLEN", "h"]), 2);
}

#[test]
fn hset_arity_errors_including_odd_tails() {
    let db = TestDb::new();
    assert_wrong_args(db.exec(&["HSET"]), "hset");
    assert_wrong_args(db.exec(&["HSET", "h"]), "hset");
    assert_wrong_args(db.exec(&["HSET", "h", "f"]), "hset");
    // Odd field/value tail reports under the invoked alias.
    assert_wrong_args(db.exec(&["HSET", "h", "a", "1", "b"]), "hset");
    assert_wrong_args(db.exec(&["HMSET", "h", "a", "1", "b"]), "hmset");
    // A failed HSET must not create the key.
    assert_int(db.exec(&["EXISTS", "h"]), 0);
}

#[test]
fn hset_against_string_is_wrongtype() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "s", "v"]));
    assert_wrong_type(db.exec(&["HSET", "s", "f", "v"]));
    assert_wrong_type(db.exec(&["HMSET", "s", "f", "v"]));
}

// ---- HGET / HMGET -------------------------------------------------------------

#[test]
fn hget_normal_and_missing() {
    let db = TestDb::new();
    assert_int(db.exec(&["HSET", "h", "f", "v"]), 1);
    assert_bulk(db.exec(&["HGET", "h", "f"]), "v");
    assert_null(db.exec(&["HGET", "h", "missing-field"]));
    assert_null(db.exec(&["HGET", "missing-key", "f"]));
    assert_wrong_args(db.exec(&["HGET", "h"]), "hget");
    assert_wrong_args(db.exec(&["HGET", "h", "f", "extra"]), "hget");
}

#[test]
fn hget_against_string_is_wrongtype() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "s", "v"]));
    assert_wrong_type(db.exec(&["HGET", "s", "f"]));
    assert_wrong_type(db.exec(&["HMGET", "s", "f"]));
    assert_wrong_type(db.exec(&["HGETALL", "s"]));
    assert_wrong_type(db.exec(&["HDEL", "s", "f"]));
    assert_wrong_type(db.exec(&["HLEN", "s"]));
    assert_wrong_type(db.exec(&["HEXISTS", "s", "f"]));
}

#[test]
fn hmget_preserves_field_order_with_nulls_for_missing() {
    let db = TestDb::new();
    assert_int(db.exec(&["HSET", "h", "gold", "100", "level", "5"]), 2);
    assert_eq!(
        db.exec(&["HMGET", "h", "gold", "level", "missing"]),
        Frame::Array(vec![
            Frame::Bulk(Bytes::from_static(b"100")),
            Frame::Bulk(Bytes::from_static(b"5")),
            Frame::Null,
        ])
    );
    // Missing key -> all nulls (not an error).
    assert_eq!(
        db.exec(&["HMGET", "nope", "a", "b"]),
        Frame::Array(vec![Frame::Null, Frame::Null])
    );
    assert_wrong_args(db.exec(&["HMGET", "h"]), "hmget");
}

// ---- HGETALL ------------------------------------------------------------------

#[test]
fn hgetall_resp2_is_a_flat_array() {
    let db = TestDb::new();
    assert_int(db.exec(&["HSET", "h", "a", "1", "b", "2"]), 2);
    let map = flat_to_map(db.exec(&["HGETALL", "h"]));
    assert_eq!(map.len(), 2);
    assert_eq!(map[b"a".as_slice()], b"1");
    assert_eq!(map[b"b".as_slice()], b"2");
    // Missing key -> empty array.
    assert_eq!(db.exec(&["HGETALL", "nope"]), Frame::Array(vec![]));
    assert_wrong_args(db.exec(&["HGETALL"]), "hgetall");
}

#[test]
fn hgetall_resp3_is_a_native_map() {
    let db = TestDb::new();
    assert_int(db.exec(&["HSET", "h", "a", "1", "b", "2"]), 2);
    let Frame::Map(pairs) = db.exec_v(RespVersion::V3, &["HGETALL", "h"]) else {
        panic!("expected RESP3 map");
    };
    let map: HashMap<Vec<u8>, Vec<u8>> = pairs
        .into_iter()
        .map(|(f, v)| match (f, v) {
            (Frame::Bulk(f), Frame::Bulk(v)) => (f.to_vec(), v.to_vec()),
            other => panic!("expected bulk pair, got {other:?}"),
        })
        .collect();
    assert_eq!(map.len(), 2);
    assert_eq!(map[b"a".as_slice()], b"1");
    // Missing key -> empty map under RESP3.
    assert_eq!(
        db.exec_v(RespVersion::V3, &["HGETALL", "nope"]),
        Frame::Map(vec![])
    );
}

// ---- HDEL / HLEN / HEXISTS ------------------------------------------------------

#[test]
fn hdel_counts_removed_fields_and_drops_empty_hashes() {
    let db = TestDb::new();
    assert_int(db.exec(&["HSET", "h", "a", "1", "b", "2", "c", "3"]), 3);
    assert_int(db.exec(&["HDEL", "h", "a", "missing", "b"]), 2);
    assert_int(db.exec(&["HLEN", "h"]), 1);
    // Removing the last field deletes the key entirely.
    assert_int(db.exec(&["HDEL", "h", "c"]), 1);
    assert_int(db.exec(&["EXISTS", "h"]), 0);
    assert_simple(db.exec(&["TYPE", "h"]), "none");
    // HDEL on a missing key returns 0.
    assert_int(db.exec(&["HDEL", "h", "a"]), 0);
    assert_wrong_args(db.exec(&["HDEL", "h"]), "hdel");
}

#[test]
fn hlen_and_hexists_normal_and_missing() {
    let db = TestDb::new();
    assert_int(db.exec(&["HSET", "h", "hp", "42"]), 1);
    assert_int(db.exec(&["HLEN", "h"]), 1);
    assert_int(db.exec(&["HLEN", "missing"]), 0);
    assert_int(db.exec(&["HEXISTS", "h", "hp"]), 1);
    assert_int(db.exec(&["HEXISTS", "h", "mana"]), 0);
    assert_int(db.exec(&["HEXISTS", "missing", "hp"]), 0);
    assert_wrong_args(db.exec(&["HLEN"]), "hlen");
    assert_wrong_args(db.exec(&["HEXISTS", "h"]), "hexists");
}

// ---- binary safety ---------------------------------------------------------------

#[test]
fn hash_fields_and_values_are_binary_safe() {
    let db = TestDb::new();
    let field = b"\x00\xfffield";
    let value = b"va\r\nlue\x00";
    let args: Vec<Bytes> = vec![
        Bytes::from_static(b"HSET"),
        Bytes::from_static(b"h"),
        Bytes::from_static(field),
        Bytes::from_static(value),
    ];
    let mut ctx = gamestore_datamodel::ExecCtx::new(&db.store, RespVersion::V2);
    assert_eq!(db.registry.dispatch(&mut ctx, &args), Frame::Integer(1));

    let get: Vec<Bytes> = vec![
        Bytes::from_static(b"HGET"),
        Bytes::from_static(b"h"),
        Bytes::from_static(field),
    ];
    assert_eq!(
        db.registry.dispatch(&mut ctx, &get),
        Frame::Bulk(Bytes::from_static(value))
    );
}
