//! A no-op [`Wal`] for running the DataNode with durability disabled.
//!
//! [`NullWal`] discards everything: it assigns monotonic LSNs so callers behave
//! identically, but writes nothing to disk, never `fsync`s, and replays nothing.
//! It exists so the WAL can be toggled off (`[wal] enabled = false`) to measure
//! the fsync cost of the write path against the pre-WAL baseline — the
//! "decide with benchmark data" covenant from MR-0020. **It provides no crash
//! durability** and must not be used where "do not lose confirmed writes" holds.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::Result;
use crate::record::{Lsn, WalRecord};
use crate::wal::{Replayed, Wal};

/// A [`Wal`] that durably persists nothing (see module docs).
#[derive(Debug, Default)]
pub struct NullWal {
    next_lsn: AtomicU64,
}

impl NullWal {
    /// Create a disabled WAL whose first assigned LSN is 1.
    pub fn new() -> Self {
        NullWal {
            next_lsn: AtomicU64::new(1),
        }
    }
}

impl Wal for NullWal {
    fn append(&self, records: &[WalRecord]) -> Result<Lsn> {
        let n = records.len().max(1) as u64;
        // Reserve `n` LSNs; return the last one assigned.
        let start = self.next_lsn.fetch_add(n, Ordering::Relaxed);
        Ok(start + n - 1)
    }

    fn sync(&self) -> Result<()> {
        Ok(())
    }

    fn replay(&self, _from: Lsn) -> Result<Vec<Replayed>> {
        Ok(Vec::new())
    }

    fn truncate(&self, _upto: Lsn) -> Result<()> {
        Ok(())
    }

    fn next_lsn(&self) -> Lsn {
        self.next_lsn.load(Ordering::Relaxed)
    }

    fn pending_bytes(&self) -> u64 {
        0
    }

    fn fsync_count(&self) -> u64 {
        0
    }
}
