//! The [`Wal`] abstraction (plan §2.3).
//!
//! A write-ahead log is the per-Core durability primitive: writes are appended
//! here and made durable (`fsync`) **before** they are applied to the engine,
//! so a crash can never lose an acknowledged write (`docs/design/03-storage-engine.md`
//! §6/§8). One Core's Replicas share one WAL to merge fragmented commits into
//! fewer `fsync`s; today a Core is a single store, but the interface already
//! carries a partition id per record to leave room for that (see
//! [`crate::record::WalRecord`]).

use crate::error::Result;
use crate::record::{Lsn, WalRecord};

/// One replayed record together with the LSN it was assigned at append time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replayed {
    /// The log sequence number this record was appended at.
    pub lsn: Lsn,
    /// The recovered record.
    pub record: WalRecord,
}

/// A per-Core write-ahead log.
///
/// Implementations must be cheap to share across threads (`Send + Sync`): the
/// file implementation ([`crate::file::FileWal`]) is used behind an `Arc` and
/// its `&self` methods are internally synchronized, so concurrent writers'
/// `sync` calls coalesce into a single `fsync` (group commit).
pub trait Wal: Send + Sync {
    /// Append `records` to the log (buffered to the OS, **not** yet fsync'd),
    /// returning the [`Lsn`] assigned to the last record. Call [`Wal::sync`] to
    /// make appended records durable.
    fn append(&self, records: &[WalRecord]) -> Result<Lsn>;

    /// Make every record appended so far durable (`fsync`). Concurrent calls
    /// coalesce so a batch of writers costs a single `fsync` (group commit).
    fn sync(&self) -> Result<()>;

    /// Replay every durably-logged record with [`Lsn`] `>= from`, in append
    /// order. A torn or CRC-corrupt tail stops iteration (and is truncated off
    /// the file) rather than erroring — see [`crate::file`].
    fn replay(&self, from: Lsn) -> Result<Vec<Replayed>>;

    /// Discard log records with [`Lsn`] `< upto` (garbage-collect after their
    /// effects are durably in the engine). Whole segments below `upto` are
    /// removed; the segment containing `upto` and the active segment are kept.
    fn truncate(&self, upto: Lsn) -> Result<()>;

    /// The LSN that will be assigned to the next appended record.
    fn next_lsn(&self) -> Lsn;

    /// Bytes of log currently retained and awaiting GC (`wal_gc_pending`).
    fn pending_bytes(&self) -> u64;

    /// Total number of `fsync` syscalls issued so far (group-commit evidence).
    fn fsync_count(&self) -> u64;
}
