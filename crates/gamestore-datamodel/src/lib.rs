//! `gamestore-datamodel` — Redis type / command layer (plan I-04).
//!
//! This crate translates parsed Redis requests (argument vectors produced by
//! [`gamestore_protocol`]) into [`gamestore_engine::Store`] operations and
//! engine results back into RESP reply [`Frame`]s:
//!
//! - [`registry`] — the [`CommandRegistry`] (case-insensitive name → handler +
//!   Redis-style arity check) and the [`CommandHandler`] / [`ExecCtx`]
//!   abstractions from the plan (§2.3).
//! - [`commands`] — the command set: connectivity (`PING`/`ECHO`),
//!   String + TTL (`SET`/`GET`/`DEL`/`EXISTS`/`TYPE`/`EXPIRE`/`PEXPIRE`/`TTL`/
//!   `PTTL`), Hash (`HSET`/`HMSET`/`HGET`/`HMGET`/`HGETALL`/`HDEL`/`HLEN`/
//!   `HEXISTS`) from I-04; Set (`SADD`/`SREM`/`SISMEMBER`/`SMEMBERS`/`SCARD`),
//!   ZSet (`ZADD`/`ZSCORE`/`ZRANGE`/`ZRANGEBYSCORE`/`ZREM`/`ZCARD`) and List
//!   (`LPUSH`/`RPUSH`/`LPOP`/`RPOP`/`LRANGE`/`LLEN`) from I-06; and the
//!   `DBSIZE`/`RAWCOUNT`/`COMPACT` introspection used by the consistency
//!   tests.
//!
//! Expiry is lazy and lives in the engine ([`docs/design/03-storage-engine.md`]
//! §3); this layer only converts relative `EX`/`PX`/`EXPIRE` inputs into
//! absolute unix-epoch milliseconds (and `PTTL` results back into seconds for
//! `TTL`). Error message wording follows Redis (`ERR wrong number of arguments
//! for 'xxx' command`, `WRONGTYPE ...`) so standard clients and test suites see
//! familiar errors.
//!
//! Connection-scoped commands (`HELLO`, `QUIT`, `CLIENT`, …) and admin verbs
//! that mutate the whole database (`FLUSHDB`) stay in the DataNode assembly
//! (I-05): they concern the connection/server, not the data model.
//!
//! [`Frame`]: gamestore_protocol::Frame
#![forbid(unsafe_code)]

pub mod commands;
pub mod registry;

pub use registry::{CommandHandler, CommandRegistry, ExecCtx};
