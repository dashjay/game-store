//! Shared helpers for the datamodel integration tests: a fresh RocksDB-backed
//! [`Store`] + the standard [`CommandRegistry`], with a thin `exec` wrapper so
//! assertions read like Redis sessions.
#![allow(dead_code)] // not every test binary uses every helper

use bytes::Bytes;
use gamestore_datamodel::{CommandRegistry, ExecCtx};
use gamestore_engine::{EngineConfig, RocksEngine, Store};
use gamestore_protocol::{Frame, RespVersion};
use tempfile::TempDir;

/// A fresh store + registry per test (the moral equivalent of `FLUSHDB`, which
/// itself only lands with the I-05 server assembly).
pub struct TestDb {
    pub store: Store<RocksEngine>,
    pub registry: CommandRegistry<RocksEngine>,
    _dir: TempDir,
}

impl TestDb {
    pub fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let store = Store::open(dir.path(), &EngineConfig::default()).expect("open store");
        TestDb {
            store,
            registry: CommandRegistry::standard(),
            _dir: dir,
        }
    }

    /// Dispatch one command at the given protocol version.
    pub fn exec_v(&self, version: RespVersion, args: &[&str]) -> Frame {
        let args: Vec<Bytes> = args
            .iter()
            .map(|s| Bytes::copy_from_slice(s.as_bytes()))
            .collect();
        let mut ctx = ExecCtx::new(&self.store, version);
        self.registry.dispatch(&mut ctx, &args)
    }

    /// Dispatch one command as a RESP2 client (the default for most tests).
    pub fn exec(&self, args: &[&str]) -> Frame {
        self.exec_v(RespVersion::V2, args)
    }
}

// ---- assertion helpers ------------------------------------------------------

pub fn assert_ok(frame: Frame) {
    assert_eq!(frame, Frame::ok());
}

pub fn assert_int(frame: Frame, want: i64) {
    assert_eq!(frame, Frame::Integer(want));
}

pub fn assert_bulk(frame: Frame, want: &str) {
    assert_eq!(frame, Frame::Bulk(Bytes::copy_from_slice(want.as_bytes())));
}

pub fn assert_simple(frame: Frame, want: &str) {
    assert_eq!(frame, Frame::Simple(want.to_string()));
}

pub fn assert_null(frame: Frame) {
    assert_eq!(frame, Frame::Null);
}

/// Assert an error reply whose message starts with `prefix`, returning the
/// full message for further checks.
pub fn assert_err_prefix(frame: Frame, prefix: &str) -> String {
    match frame {
        Frame::Error(msg) => {
            assert!(
                msg.starts_with(prefix),
                "error {msg:?} does not start with {prefix:?}"
            );
            msg
        }
        other => panic!("expected error reply, got {other:?}"),
    }
}

/// Assert the canonical Redis arity error for `cmd` (lowercase name).
pub fn assert_wrong_args(frame: Frame, cmd: &str) {
    assert_eq!(
        frame,
        Frame::Error(format!("ERR wrong number of arguments for '{cmd}' command"))
    );
}

/// Assert the Redis `WRONGTYPE` error.
pub fn assert_wrong_type(frame: Frame) {
    assert_err_prefix(frame, "WRONGTYPE");
}

/// Unwrap an integer reply (for range assertions like PTTL bounds).
pub fn int_of(frame: Frame) -> i64 {
    match frame {
        Frame::Integer(n) => n,
        other => panic!("expected integer reply, got {other:?}"),
    }
}
