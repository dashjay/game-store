//! Per-command tests for the String + TTL family: normal, boundary and error
//! paths (arity, WRONGTYPE, missing keys, TTL edges).

mod common;

use common::*;

// ---- SET / GET --------------------------------------------------------------

#[test]
fn set_get_roundtrip_and_overwrite() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "k1", "v1"]));
    assert_bulk(db.exec(&["GET", "k1"]), "v1");
    assert_ok(db.exec(&["SET", "k1", "v2"]));
    assert_bulk(db.exec(&["GET", "k1"]), "v2");
}

#[test]
fn get_missing_key_is_null() {
    let db = TestDb::new();
    assert_null(db.exec(&["GET", "nope"]));
}

#[test]
fn set_arity_errors() {
    let db = TestDb::new();
    assert_wrong_args(db.exec(&["SET"]), "set");
    assert_wrong_args(db.exec(&["SET", "k"]), "set");
}

#[test]
fn set_overwrites_a_hash_without_wrongtype() {
    // SET replaces any previous value regardless of type, like Redis.
    let db = TestDb::new();
    assert_int(db.exec(&["HSET", "k", "f", "v"]), 1);
    assert_ok(db.exec(&["SET", "k", "s"]));
    assert_simple(db.exec(&["TYPE", "k"]), "string");
    assert_bulk(db.exec(&["GET", "k"]), "s");
}

#[test]
fn get_against_hash_is_wrongtype() {
    let db = TestDb::new();
    assert_int(db.exec(&["HSET", "h", "f", "v"]), 1);
    assert_wrong_type(db.exec(&["GET", "h"]));
}

// ---- SET expire options -----------------------------------------------------

#[test]
fn set_with_ex_and_px_sets_a_ttl() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "k", "v", "EX", "100"]));
    let ttl = int_of(db.exec(&["TTL", "k"]));
    assert!(ttl > 0 && ttl <= 100, "ttl={ttl}");

    assert_ok(db.exec(&["SET", "k2", "v", "PX", "100000"]));
    let pttl = int_of(db.exec(&["PTTL", "k2"]));
    assert!(pttl > 0 && pttl <= 100_000, "pttl={pttl}");

    // Option names are case-insensitive.
    assert_ok(db.exec(&["SET", "k3", "v", "ex", "100"]));
    let ttl3 = int_of(db.exec(&["TTL", "k3"]));
    assert!(ttl3 > 0 && ttl3 <= 100, "ttl={ttl3}");
}

#[test]
fn set_overwrite_clears_previous_ttl() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "k", "v", "EX", "100"]));
    assert_ok(db.exec(&["SET", "k", "v2"]));
    assert_int(db.exec(&["TTL", "k"]), -1);
}

#[test]
fn set_expire_option_error_paths() {
    let db = TestDb::new();
    // Missing option value / unknown option / EX+PX together: syntax error.
    assert_err_prefix(db.exec(&["SET", "k", "v", "EX"]), "ERR syntax error");
    assert_err_prefix(db.exec(&["SET", "k", "v", "NX"]), "ERR syntax error");
    assert_err_prefix(
        db.exec(&["SET", "k", "v", "EX", "10", "PX", "10000"]),
        "ERR syntax error",
    );
    // Non-integer expire value.
    assert_err_prefix(
        db.exec(&["SET", "k", "v", "EX", "abc"]),
        "ERR value is not an integer or out of range",
    );
    // Zero / negative expire values are rejected like Redis.
    assert_err_prefix(
        db.exec(&["SET", "k", "v", "EX", "0"]),
        "ERR invalid expire time in 'set' command",
    );
    assert_err_prefix(
        db.exec(&["SET", "k", "v", "PX", "-5"]),
        "ERR invalid expire time in 'set' command",
    );
    // A failed SET must not clobber existing data.
    assert_ok(db.exec(&["SET", "k", "keep"]));
    assert_err_prefix(db.exec(&["SET", "k", "v", "EX", "0"]), "ERR invalid");
    assert_bulk(db.exec(&["GET", "k"]), "keep");
}

#[test]
fn set_px_expiry_is_lazy_but_observable() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "t1", "x", "PX", "80"]));
    let pttl = int_of(db.exec(&["PTTL", "t1"]));
    assert!(pttl > 0 && pttl <= 80, "pttl={pttl}");
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_null(db.exec(&["GET", "t1"]));
    assert_int(db.exec(&["EXISTS", "t1"]), 0);
    assert_int(db.exec(&["TTL", "t1"]), -2);
}

// ---- DEL / EXISTS / TYPE ----------------------------------------------------

#[test]
fn del_counts_only_existing_keys() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "a", "1"]));
    assert_ok(db.exec(&["SET", "b", "2"]));
    assert_int(db.exec(&["HSET", "h", "f", "v"]), 1);
    // DEL works across types and skips missing keys.
    assert_int(db.exec(&["DEL", "a", "missing", "b", "h"]), 3);
    assert_int(db.exec(&["EXISTS", "a", "b", "h"]), 0);
}

#[test]
fn exists_counts_each_mention() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "k", "v"]));
    assert_int(db.exec(&["EXISTS", "k"]), 1);
    assert_int(db.exec(&["EXISTS", "k", "k", "missing"]), 2);
    assert_int(db.exec(&["EXISTS", "missing"]), 0);
    assert_wrong_args(db.exec(&["EXISTS"]), "exists");
}

#[test]
fn type_reports_string_hash_none() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "s", "v"]));
    assert_int(db.exec(&["HSET", "h", "f", "v"]), 1);
    assert_simple(db.exec(&["TYPE", "s"]), "string");
    assert_simple(db.exec(&["TYPE", "h"]), "hash");
    assert_simple(db.exec(&["TYPE", "missing"]), "none");
}

// ---- EXPIRE / PEXPIRE / TTL / PTTL -------------------------------------------

#[test]
fn expire_sets_ttl_and_reports_existence() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "k", "v"]));
    assert_int(db.exec(&["EXPIRE", "k", "100"]), 1);
    let ttl = int_of(db.exec(&["TTL", "k"]));
    assert!(ttl > 0 && ttl <= 100, "ttl={ttl}");
    // TTL is reported in seconds rounded *up*: right after EXPIRE 100 the
    // remaining ~99.9s must read as 100, not 99.
    assert_eq!(ttl, 100);
    assert_int(db.exec(&["EXPIRE", "missing", "100"]), 0);
}

#[test]
fn pexpire_uses_milliseconds() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "k", "v"]));
    assert_int(db.exec(&["PEXPIRE", "k", "100000"]), 1);
    let pttl = int_of(db.exec(&["PTTL", "k"]));
    assert!(pttl > 0 && pttl <= 100_000, "pttl={pttl}");
    assert_int(db.exec(&["PEXPIRE", "missing", "1000"]), 0);
}

#[test]
fn expire_applies_to_hashes_too() {
    let db = TestDb::new();
    assert_int(db.exec(&["HSET", "h", "f", "v"]), 1);
    assert_int(db.exec(&["EXPIRE", "h", "100"]), 1);
    let ttl = int_of(db.exec(&["TTL", "h"]));
    assert!(ttl > 0 && ttl <= 100, "ttl={ttl}");
}

#[test]
fn ttl_and_pttl_edge_values() {
    let db = TestDb::new();
    assert_int(db.exec(&["TTL", "missing"]), -2);
    assert_int(db.exec(&["PTTL", "missing"]), -2);
    assert_ok(db.exec(&["SET", "k", "v"]));
    assert_int(db.exec(&["TTL", "k"]), -1);
    assert_int(db.exec(&["PTTL", "k"]), -1);
}

#[test]
fn expire_with_non_positive_amount_deletes_the_key() {
    // Redis: a non-positive EXPIRE deletes the key and still replies 1.
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "k", "v"]));
    assert_int(db.exec(&["EXPIRE", "k", "0"]), 1);
    assert_null(db.exec(&["GET", "k"]));
    assert_int(db.exec(&["EXISTS", "k"]), 0);

    assert_ok(db.exec(&["SET", "k2", "v"]));
    assert_int(db.exec(&["PEXPIRE", "k2", "-1000"]), 1);
    assert_int(db.exec(&["EXISTS", "k2"]), 0);
}

#[test]
fn expire_error_paths() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "k", "v"]));
    assert_wrong_args(db.exec(&["EXPIRE", "k"]), "expire");
    assert_wrong_args(db.exec(&["PEXPIRE", "k"]), "pexpire");
    assert_err_prefix(
        db.exec(&["EXPIRE", "k", "abc"]),
        "ERR value is not an integer or out of range",
    );
    // Multiplying i64::MAX seconds by 1000 overflows -> invalid expire time.
    assert_err_prefix(
        db.exec(&["EXPIRE", "k", &i64::MAX.to_string()]),
        "ERR invalid expire time in 'expire' command",
    );
    assert_wrong_args(db.exec(&["TTL"]), "ttl");
    assert_wrong_args(db.exec(&["PTTL", "k", "extra"]), "pttl");
}

#[test]
fn ttl_rounds_milliseconds_up_to_seconds() {
    let db = TestDb::new();
    assert_ok(db.exec(&["SET", "k", "v", "PX", "1500"]));
    // 1500ms remaining reads as ceil(1.5s) = 2s.
    assert_eq!(int_of(db.exec(&["TTL", "k"])), 2);
}

#[test]
fn lazily_expired_key_is_gone_from_all_read_paths() {
    let db = TestDb::new();
    // Set an absolute expiry in the past directly through the store to avoid
    // sleeping: the command layer must observe it as missing everywhere.
    db.store.set(b"k", b"v", 1).unwrap();
    assert_null(db.exec(&["GET", "k"]));
    assert_simple(db.exec(&["TYPE", "k"]), "none");
    assert_int(db.exec(&["EXISTS", "k"]), 0);
    assert_int(db.exec(&["DEL", "k"]), 0);
    assert_int(db.exec(&["EXPIRE", "k", "100"]), 0);

    // Sanity: now_ms-based deadlines from the command layer stay in the future.
    assert_ok(db.exec(&["SET", "k2", "v", "EX", "100"]));
    assert!(db.store.pttl(b"k2").unwrap() > 0);
}
