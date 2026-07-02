//! Per-command tests for the ZSet family: normal, boundary and error paths
//! (arity, score syntax, exclusive/infinite bounds, WRONGTYPE), plus the
//! RESP2 vs RESP3 reply shapes (bulk-string vs native-double scores).

mod common;

use bytes::Bytes;
use common::*;
use gamestore_protocol::{Frame, RespVersion};

fn bulk_frame(s: &str) -> Frame {
    Frame::Bulk(Bytes::copy_from_slice(s.as_bytes()))
}

// ---- ZADD ----------------------------------------------------------------------

#[test]
fn zadd_counts_new_members_and_updates_scores() {
    let db = TestDb::new();
    assert_int(db.exec(&["ZADD", "z", "1", "a", "2", "b"]), 2);
    // Score update of an existing member counts zero.
    assert_int(db.exec(&["ZADD", "z", "5", "a"]), 0);
    assert_bulk(db.exec(&["ZSCORE", "z", "a"]), "5");
    // Duplicate member in one call: last score wins, counted once.
    assert_int(db.exec(&["ZADD", "z", "1", "c", "9", "c"]), 1);
    assert_bulk(db.exec(&["ZSCORE", "z", "c"]), "9");
    assert_int(db.exec(&["ZCARD", "z"]), 3);
    assert_simple(db.exec(&["TYPE", "z"]), "zset");
}

#[test]
fn zadd_score_syntax_and_arity_errors() {
    let db = TestDb::new();
    assert_wrong_args(db.exec(&["ZADD", "z", "1"]), "zadd");
    // Odd score/member tail is a syntax error in Redis.
    assert_err_prefix(db.exec(&["ZADD", "z", "1", "a", "2"]), "ERR syntax error");
    assert_err_prefix(
        db.exec(&["ZADD", "z", "notanumber", "a"]),
        "ERR value is not a valid float",
    );
    assert_err_prefix(
        db.exec(&["ZADD", "z", "nan", "a"]),
        "ERR value is not a valid float",
    );
    // Unsupported flags are rejected loudly, not silently misparsed.
    assert_err_prefix(db.exec(&["ZADD", "z", "NX", "1", "a"]), "ERR syntax error");
    assert_err_prefix(db.exec(&["ZADD", "z", "GT", "1", "a"]), "ERR syntax error");
    // Nothing was created by the failed calls.
    assert_int(db.exec(&["EXISTS", "z"]), 0);
}

#[test]
fn zadd_accepts_infinity_and_scientific_notation() {
    let db = TestDb::new();
    assert_int(
        db.exec(&["ZADD", "z", "-inf", "low", "+inf", "high", "1e2", "mid"]),
        3,
    );
    assert_bulk(db.exec(&["ZSCORE", "z", "low"]), "-inf");
    assert_bulk(db.exec(&["ZSCORE", "z", "high"]), "inf");
    assert_bulk(db.exec(&["ZSCORE", "z", "mid"]), "100");
    assert_eq!(
        db.exec(&["ZRANGE", "z", "0", "-1"]),
        Frame::Array(vec![
            bulk_frame("low"),
            bulk_frame("mid"),
            bulk_frame("high")
        ])
    );
}

// ---- ZSCORE ---------------------------------------------------------------------

#[test]
fn zscore_shapes_and_missing() {
    let db = TestDb::new();
    assert_int(db.exec(&["ZADD", "z", "1.5", "m"]), 1);
    // RESP2: bulk string; RESP3: native double.
    assert_bulk(db.exec(&["ZSCORE", "z", "m"]), "1.5");
    assert_eq!(
        db.exec_v(RespVersion::V3, &["ZSCORE", "z", "m"]),
        Frame::Double(1.5)
    );
    assert_null(db.exec(&["ZSCORE", "z", "missing"]));
    assert_null(db.exec(&["ZSCORE", "missing", "m"]));
    assert_wrong_args(db.exec(&["ZSCORE", "z"]), "zscore");
    // Integral scores print without a decimal point (Redis convention).
    assert_int(db.exec(&["ZADD", "z", "42", "n"]), 1);
    assert_bulk(db.exec(&["ZSCORE", "z", "n"]), "42");
}

// ---- ZRANGE ---------------------------------------------------------------------

#[test]
fn zrange_ranks_negatives_and_withscores() {
    let db = TestDb::new();
    assert_int(db.exec(&["ZADD", "z", "1", "a", "2", "b", "3", "c"]), 3);
    assert_eq!(
        db.exec(&["ZRANGE", "z", "0", "-1"]),
        Frame::Array(vec![bulk_frame("a"), bulk_frame("b"), bulk_frame("c")])
    );
    assert_eq!(
        db.exec(&["ZRANGE", "z", "1", "1"]),
        Frame::Array(vec![bulk_frame("b")])
    );
    assert_eq!(
        db.exec(&["ZRANGE", "z", "-2", "-1"]),
        Frame::Array(vec![bulk_frame("b"), bulk_frame("c")])
    );
    // Empty for inverted / out-of-range ranks; missing key -> empty array.
    assert_eq!(db.exec(&["ZRANGE", "z", "5", "9"]), Frame::Array(vec![]));
    assert_eq!(
        db.exec(&["ZRANGE", "nope", "0", "-1"]),
        Frame::Array(vec![])
    );

    // WITHSCORES: RESP2 flattens member,score; RESP3 nests pairs with doubles.
    assert_eq!(
        db.exec(&["ZRANGE", "z", "0", "1", "WITHSCORES"]),
        Frame::Array(vec![
            bulk_frame("a"),
            bulk_frame("1"),
            bulk_frame("b"),
            bulk_frame("2"),
        ])
    );
    assert_eq!(
        db.exec_v(RespVersion::V3, &["ZRANGE", "z", "0", "1", "WITHSCORES"]),
        Frame::Array(vec![
            Frame::Array(vec![bulk_frame("a"), Frame::Double(1.0)]),
            Frame::Array(vec![bulk_frame("b"), Frame::Double(2.0)]),
        ])
    );

    assert_eq!(
        db.exec(&["ZRANGE", "z", "x", "-1"]),
        Frame::Error("ERR value is not an integer or out of range".into())
    );
    assert_err_prefix(
        db.exec(&["ZRANGE", "z", "0", "-1", "REV"]),
        "ERR syntax error",
    );
    assert_wrong_args(db.exec(&["ZRANGE", "z", "0"]), "zrange");
}

#[test]
fn zrange_ties_break_lexicographically() {
    let db = TestDb::new();
    assert_int(db.exec(&["ZADD", "z", "1", "bb", "1", "aa", "1", "cc"]), 3);
    assert_eq!(
        db.exec(&["ZRANGE", "z", "0", "-1"]),
        Frame::Array(vec![bulk_frame("aa"), bulk_frame("bb"), bulk_frame("cc")])
    );
}

// ---- ZRANGEBYSCORE ---------------------------------------------------------------

#[test]
fn zrangebyscore_bounds_inclusive_exclusive_infinite() {
    let db = TestDb::new();
    assert_int(db.exec(&["ZADD", "z", "1", "a", "2", "b", "3", "c"]), 3);
    assert_eq!(
        db.exec(&["ZRANGEBYSCORE", "z", "1", "2"]),
        Frame::Array(vec![bulk_frame("a"), bulk_frame("b")])
    );
    // Exclusive bounds.
    assert_eq!(
        db.exec(&["ZRANGEBYSCORE", "z", "(1", "3"]),
        Frame::Array(vec![bulk_frame("b"), bulk_frame("c")])
    );
    assert_eq!(
        db.exec(&["ZRANGEBYSCORE", "z", "(1", "(3"]),
        Frame::Array(vec![bulk_frame("b")])
    );
    // Infinite bounds.
    assert_eq!(
        db.exec(&["ZRANGEBYSCORE", "z", "-inf", "+inf"]),
        Frame::Array(vec![bulk_frame("a"), bulk_frame("b"), bulk_frame("c")])
    );
    // Empty results and missing key.
    assert_eq!(
        db.exec(&["ZRANGEBYSCORE", "z", "9", "10"]),
        Frame::Array(vec![])
    );
    assert_eq!(
        db.exec(&["ZRANGEBYSCORE", "nope", "-inf", "+inf"]),
        Frame::Array(vec![])
    );
    // WITHSCORES + LIMIT.
    assert_eq!(
        db.exec(&["ZRANGEBYSCORE", "z", "-inf", "+inf", "WITHSCORES"]),
        Frame::Array(vec![
            bulk_frame("a"),
            bulk_frame("1"),
            bulk_frame("b"),
            bulk_frame("2"),
            bulk_frame("c"),
            bulk_frame("3"),
        ])
    );
    assert_eq!(
        db.exec(&["ZRANGEBYSCORE", "z", "-inf", "+inf", "LIMIT", "1", "1"]),
        Frame::Array(vec![bulk_frame("b")])
    );
    // Negative count = "everything from offset"; negative offset = nothing.
    assert_eq!(
        db.exec(&["ZRANGEBYSCORE", "z", "-inf", "+inf", "LIMIT", "1", "-1"]),
        Frame::Array(vec![bulk_frame("b"), bulk_frame("c")])
    );
    assert_eq!(
        db.exec(&["ZRANGEBYSCORE", "z", "-inf", "+inf", "LIMIT", "-1", "2"]),
        Frame::Array(vec![])
    );
    // Errors.
    assert_err_prefix(
        db.exec(&["ZRANGEBYSCORE", "z", "abc", "2"]),
        "ERR min or max is not a float",
    );
    assert_err_prefix(
        db.exec(&["ZRANGEBYSCORE", "z", "1", "2", "NOPE"]),
        "ERR syntax error",
    );
    assert_err_prefix(
        db.exec(&["ZRANGEBYSCORE", "z", "1", "2", "LIMIT", "1"]),
        "ERR syntax error",
    );
    assert_wrong_args(db.exec(&["ZRANGEBYSCORE", "z", "1"]), "zrangebyscore");
}

// ---- ZREM / ZCARD ------------------------------------------------------------------

#[test]
fn zrem_counts_removed_and_drops_empty_zsets() {
    let db = TestDb::new();
    assert_int(db.exec(&["ZADD", "z", "1", "a", "2", "b"]), 2);
    assert_int(db.exec(&["ZREM", "z", "a", "missing"]), 1);
    assert_int(db.exec(&["ZCARD", "z"]), 1);
    // Removing the last member deletes the key.
    assert_int(db.exec(&["ZREM", "z", "b"]), 1);
    assert_int(db.exec(&["EXISTS", "z"]), 0);
    assert_simple(db.exec(&["TYPE", "z"]), "none");
    assert_int(db.exec(&["ZREM", "z", "a"]), 0);
    assert_int(db.exec(&["ZCARD", "missing"]), 0);
    assert_wrong_args(db.exec(&["ZREM", "z"]), "zrem");
    assert_wrong_args(db.exec(&["ZCARD"]), "zcard");
}

// ---- cross-type / generic -----------------------------------------------------------

#[test]
fn zset_commands_against_other_types_are_wrongtype() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "str", "v"]));
    for cmd in [
        vec!["ZADD", "str", "1", "m"],
        vec!["ZSCORE", "str", "m"],
        vec!["ZRANGE", "str", "0", "-1"],
        vec!["ZRANGEBYSCORE", "str", "-inf", "+inf"],
        vec!["ZREM", "str", "m"],
        vec!["ZCARD", "str"],
    ] {
        assert_wrong_type(db.exec(&cmd));
    }
}

#[test]
fn generic_commands_apply_to_zsets() {
    let db = TestDb::new();
    assert_int(db.exec(&["ZADD", "z", "1", "m"]), 1);
    assert_int(db.exec(&["EXISTS", "z"]), 1);
    assert_int(db.exec(&["EXPIRE", "z", "100"]), 1);
    let ttl = int_of(db.exec(&["TTL", "z"]));
    assert!(ttl > 0 && ttl <= 100, "ttl={ttl}");
    assert_int(db.exec(&["DEL", "z"]), 1);
    assert_int(db.exec(&["EXISTS", "z"]), 0);
}
