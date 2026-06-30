//! Version-based garbage collection for stale subkeys.
//!
//! `docs/design/03-storage-engine.md` §2/§4: deleting or rebuilding a Hash is an
//! O(1) `version` bump; the old version's subkeys become garbage and are
//! physically reclaimed by a RocksDB Compaction Filter in the background,
//! decoupled from the foreground request path.
//!
//! To make the decision without a re-entrant DB lookup inside the compaction
//! thread (and to keep the Rust and C++ spikes byte-for-byte equivalent), we
//! keep a small in-memory map of `user_key -> current structure version` — this
//! is exactly the kind of metadata/version cache the design already calls for.
//! The compaction filter keeps a subkey iff its owner is still present and the
//! subkey's version equals the owner's current version.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::encoding;

#[derive(Clone, Default)]
pub struct VersionMap {
    inner: Arc<RwLock<HashMap<Vec<u8>, u64>>>,
}

impl VersionMap {
    pub fn new() -> Self {
        VersionMap {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn set(&self, user_key: &[u8], version: u64) {
        self.inner.write().unwrap().insert(user_key.to_vec(), version);
    }

    pub fn remove(&self, user_key: &[u8]) {
        self.inner.write().unwrap().remove(user_key);
    }

    pub fn get(&self, user_key: &[u8]) -> Option<u64> {
        self.inner.read().unwrap().get(user_key).copied()
    }

    /// Compaction-filter predicate: should this raw record be kept?
    /// Metadata records are always kept here (TTL/version lifecycle for them is
    /// handled on the foreground path); only stale subkeys are dropped.
    pub fn should_keep(&self, key: &[u8]) -> bool {
        match encoding::parse_subkey(key) {
            None => true, // not a subkey (e.g. metadata) -> keep
            Some((user_key, version, _field)) => match self.get(user_key) {
                Some(current) => version == current,
                None => false, // owner deleted -> subkey is garbage
            },
        }
    }
}
