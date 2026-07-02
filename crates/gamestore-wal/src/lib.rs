//! `gamestore-wal` — the per-Core write-ahead log (I-08).
//!
//! This crate provides GameStore's durability primitive: writes are appended
//! to a segmented, CRC-checked, group-committing file log and made durable with
//! `fsync` **before** they are applied to the storage engine, so an
//! acknowledged write is never lost across a crash
//! ([`docs/design/03-storage-engine.md`] §6/§8). It contains:
//!
//! - [`Wal`] — the `append` / `sync` / `replay` / `truncate` abstraction
//!   (plan §2.3), with [`WalRecord`] / [`WalOp`] as the redo model.
//! - [`FileWal`] — the concrete segmented file log: group-commit `fsync`
//!   (concurrent writers share one syscall), per-record CRC32, and crash
//!   recovery that truncates a torn/corrupt tail into a clean prefix.
//! - [`WalEngine`] — a [`gamestore_engine::GeneralEngine`] decorator that logs
//!   every write before applying it, replays the log on startup, and
//!   checkpoints (engine flush + log GC) to bound the log size.
//!
//! # Single Core / shared WAL
//!
//! Today a DataNode is one logical Core with a single [`gamestore_engine::Store`],
//! so there is one [`WalEngine`] over one [`FileWal`]. The design's "multiple
//! Replicas per Core share one WAL" is left as an interface seam: the WAL is an
//! `Arc<dyn Wal>` (shareable) and every [`WalRecord`] already carries a
//! reserved `partition` id, so adding replicas later needs no format change.
#![forbid(unsafe_code)]

pub mod engine;
pub mod error;
pub mod file;
pub mod null;
pub mod record;
pub mod wal;

pub use engine::WalEngine;
pub use error::{Result, WalError};
pub use file::{FileWal, WalConfig, DEFAULT_SEGMENT_MAX_BYTES};
pub use null::NullWal;
pub use record::{Lsn, WalOp, WalRecord};
pub use wal::{Replayed, Wal};
