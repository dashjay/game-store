//! CommandRegistry contract tests: case-insensitive lookup, arity validation
//! (exact and "at least" specs), unknown/empty command handling.

mod common;

use common::*;
use gamestore_protocol::Frame;

#[test]
fn command_names_are_case_insensitive() {
    let db = TestDb::new();
    assert_simple(db.exec(&["PING"]), "PONG");
    assert_simple(db.exec(&["ping"]), "PONG");
    assert_simple(db.exec(&["PiNg"]), "PONG");

    assert_ok(db.exec(&["set", "k", "v"]));
    assert_bulk(db.exec(&["GET", "k"]), "v");
    assert_bulk(db.exec(&["get", "k"]), "v");
}

#[test]
fn unknown_command_is_reported_with_original_spelling() {
    let db = TestDb::new();
    let msg = assert_err_prefix(db.exec(&["ZADD", "k", "1", "m"]), "ERR unknown command");
    assert!(msg.contains("'ZADD'"), "got {msg:?}");
}

#[test]
fn empty_command_is_an_error() {
    let db = TestDb::new();
    assert_err_prefix(db.exec(&[]), "ERR empty command");
}

#[test]
fn exact_arity_is_enforced_before_the_handler_runs() {
    let db = TestDb::new();
    // GET has arity 2: both too few and too many arguments are rejected.
    assert_wrong_args(db.exec(&["GET"]), "get");
    assert_wrong_args(db.exec(&["GET", "k", "extra"]), "get");
    // TYPE has arity 2 as well.
    assert_wrong_args(db.exec(&["TYPE"]), "type");
    assert_wrong_args(db.exec(&["TYPE", "k", "extra"]), "type");
}

#[test]
fn minimum_arity_allows_variadic_tails() {
    let db = TestDb::new();
    // DEL is arity -2: one key or many, but zero keys is an error.
    assert_wrong_args(db.exec(&["DEL"]), "del");
    assert_int(db.exec(&["DEL", "a"]), 0);
    assert_int(db.exec(&["DEL", "a", "b", "c"]), 0);
}

#[test]
fn contains_is_case_insensitive() {
    let db = TestDb::new();
    assert!(db.registry.contains(b"get"));
    assert!(db.registry.contains(b"GET"));
    assert!(db.registry.contains(b"HGetAll"));
    assert!(!db.registry.contains(b"ZADD"));
}

#[test]
fn arity_error_uses_lowercase_name_even_for_uppercase_invocation() {
    let db = TestDb::new();
    assert_eq!(
        db.exec(&["HGET", "k"]),
        Frame::Error("ERR wrong number of arguments for 'hget' command".into())
    );
}
