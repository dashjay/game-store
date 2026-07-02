//! Per-command tests for the List family: normal, boundary and error paths
//! (arity, negative indexes, LPOP/RPOP count semantics, WRONGTYPE).

mod common;

use bytes::Bytes;
use common::*;
use gamestore_protocol::Frame;

fn bulk_frame(s: &str) -> Frame {
    Frame::Bulk(Bytes::copy_from_slice(s.as_bytes()))
}

fn assert_list(frame: Frame, want: &[&str]) {
    assert_eq!(
        frame,
        Frame::Array(want.iter().map(|s| bulk_frame(s)).collect::<Vec<_>>())
    );
}

// ---- LPUSH / RPUSH ---------------------------------------------------------------

#[test]
fn push_returns_length_and_orders_correctly() {
    let db = TestDb::new();
    // RPUSH appends left-to-right; LPUSH prepends one by one (reversing).
    assert_int(db.exec(&["RPUSH", "l", "a", "b"]), 2);
    assert_int(db.exec(&["LPUSH", "l", "x", "y"]), 4);
    assert_list(db.exec(&["LRANGE", "l", "0", "-1"]), &["y", "x", "a", "b"]);
    assert_simple(db.exec(&["TYPE", "l"]), "list");
    assert_int(db.exec(&["LLEN", "l"]), 4);
    assert_wrong_args(db.exec(&["LPUSH", "l"]), "lpush");
    assert_wrong_args(db.exec(&["RPUSH"]), "rpush");
}

// ---- LPOP / RPOP -----------------------------------------------------------------

#[test]
fn pop_without_count_returns_bulk_or_nil() {
    let db = TestDb::new();
    assert_int(db.exec(&["RPUSH", "l", "a", "b", "c"]), 3);
    assert_bulk(db.exec(&["LPOP", "l"]), "a");
    assert_bulk(db.exec(&["RPOP", "l"]), "c");
    assert_int(db.exec(&["LLEN", "l"]), 1);
    // Popping the last element deletes the key.
    assert_bulk(db.exec(&["LPOP", "l"]), "b");
    assert_int(db.exec(&["EXISTS", "l"]), 0);
    assert_simple(db.exec(&["TYPE", "l"]), "none");
    assert_null(db.exec(&["LPOP", "l"]));
    assert_null(db.exec(&["RPOP", "missing"]));
}

#[test]
fn pop_with_count_returns_array_semantics() {
    let db = TestDb::new();
    assert_int(db.exec(&["RPUSH", "l", "a", "b", "c"]), 3);
    // count pops in order from the chosen end.
    assert_list(db.exec(&["LPOP", "l", "2"]), &["a", "b"]);
    assert_int(db.exec(&["RPUSH", "l", "d", "e"]), 3);
    assert_list(db.exec(&["RPOP", "l", "2"]), &["e", "d"]);
    // count larger than the list drains it (and deletes the key).
    assert_list(db.exec(&["LPOP", "l", "99"]), &["c"]);
    assert_int(db.exec(&["EXISTS", "l"]), 0);
    // count on a missing key -> nil; count 0 on an existing key -> empty array.
    assert_null(db.exec(&["LPOP", "l", "2"]));
    assert_int(db.exec(&["RPUSH", "l", "x"]), 1);
    assert_list(db.exec(&["LPOP", "l", "0"]), &[]);
    assert_int(db.exec(&["LLEN", "l"]), 1);
    // Errors: negative or non-integer count, too many arguments.
    assert_err_prefix(
        db.exec(&["LPOP", "l", "-1"]),
        "ERR value is out of range, must be positive",
    );
    assert_err_prefix(
        db.exec(&["LPOP", "l", "abc"]),
        "ERR value is not an integer or out of range",
    );
    assert_wrong_args(db.exec(&["LPOP", "l", "1", "extra"]), "lpop");
    assert_wrong_args(db.exec(&["RPOP", "l", "1", "extra"]), "rpop");
    assert_wrong_args(db.exec(&["LPOP"]), "lpop");
}

// ---- LRANGE / LLEN ------------------------------------------------------------------

#[test]
fn lrange_indexes_and_clamping() {
    let db = TestDb::new();
    assert_int(db.exec(&["RPUSH", "l", "a", "b", "c", "d"]), 4);
    assert_list(db.exec(&["LRANGE", "l", "0", "-1"]), &["a", "b", "c", "d"]);
    assert_list(db.exec(&["LRANGE", "l", "1", "2"]), &["b", "c"]);
    assert_list(db.exec(&["LRANGE", "l", "-2", "-1"]), &["c", "d"]);
    // Out-of-bounds indexes clamp; inverted ranges are empty.
    assert_list(
        db.exec(&["LRANGE", "l", "-100", "100"]),
        &["a", "b", "c", "d"],
    );
    assert_list(db.exec(&["LRANGE", "l", "3", "1"]), &[]);
    assert_list(db.exec(&["LRANGE", "l", "9", "12"]), &[]);
    // Missing key -> empty array.
    assert_list(db.exec(&["LRANGE", "missing", "0", "-1"]), &[]);
    // Errors.
    assert_eq!(
        db.exec(&["LRANGE", "l", "x", "-1"]),
        Frame::Error("ERR value is not an integer or out of range".into())
    );
    assert_wrong_args(db.exec(&["LRANGE", "l", "0"]), "lrange");
    assert_wrong_args(db.exec(&["LRANGE", "l", "0", "1", "2"]), "lrange");
}

#[test]
fn llen_normal_and_missing() {
    let db = TestDb::new();
    assert_int(db.exec(&["LLEN", "missing"]), 0);
    assert_int(db.exec(&["RPUSH", "l", "a", "b"]), 2);
    assert_int(db.exec(&["LLEN", "l"]), 2);
    assert_wrong_args(db.exec(&["LLEN"]), "llen");
    assert_wrong_args(db.exec(&["LLEN", "l", "extra"]), "llen");
}

// ---- cross-type / generic ------------------------------------------------------------

#[test]
fn list_commands_against_other_types_are_wrongtype() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "str", "v"]));
    for cmd in [
        vec!["LPUSH", "str", "v"],
        vec!["RPUSH", "str", "v"],
        vec!["LPOP", "str"],
        vec!["RPOP", "str", "2"],
        vec!["LRANGE", "str", "0", "-1"],
        vec!["LLEN", "str"],
    ] {
        assert_wrong_type(db.exec(&cmd));
    }
    // And the reverse: a List key rejects other families.
    assert_int(db.exec(&["RPUSH", "l", "v"]), 1);
    assert_wrong_type(db.exec(&["GET", "l"]));
    assert_wrong_type(db.exec(&["SADD", "l", "m"]));
    assert_wrong_type(db.exec(&["ZCARD", "l"]));
}

#[test]
fn generic_commands_apply_to_lists() {
    let db = TestDb::new();
    assert_int(db.exec(&["RPUSH", "l", "v"]), 1);
    assert_int(db.exec(&["EXISTS", "l"]), 1);
    assert_int(db.exec(&["EXPIRE", "l", "100"]), 1);
    let ttl = int_of(db.exec(&["TTL", "l"]));
    assert!(ttl > 0 && ttl <= 100, "ttl={ttl}");
    assert_int(db.exec(&["DEL", "l"]), 1);
    assert_int(db.exec(&["EXISTS", "l"]), 0);
}
