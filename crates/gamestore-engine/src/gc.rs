//! Version-based garbage collection for stale subkeys.
//!
//! [`docs/design/03-storage-engine.md`] §2/§4: deleting or rebuilding a Hash is
//! an O(1) `version` bump; the old version's subkeys become garbage and are
//! physically reclaimed by a RocksDB Compaction Filter in the background,
//! decoupled from the foreground request path.
//!
//! To make the decision without a re-entrant DB lookup inside the compaction
//! thread, we keep a small in-memory map of `user_key -> current structure
//! version` — exactly the kind of metadata/version cache the design already
//! calls for. The compaction filter keeps a subkey iff its owner is still
//! present and the subkey's version equals the owner's current version.
//!
//! Ported and hardened from `spike/rust/src/gc.rs`: the map is now wired into
//! the [`crate::engine::GeneralEngine`] via the [`GcPredicate`] trait rather than
//! a bespoke closure, so any engine backend can reuse it.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::encoding;
use crate::engine::GcPredicate;

/// Thread-safe `user_key -> current structure version` map.
///
/// Cloning shares the underlying state (`Arc` inside), so the copy handed to the
/// engine's compaction filter and the copy used by the foreground write path
/// observe the same versions.
#[derive(Clone, Default)]
pub struct VersionMap {
    inner: Arc<RwLock<HashMap<Vec<u8>, u64>>>,
}

impl VersionMap {
    /// Create an empty version map.
    pub fn new() -> Self {
        VersionMap {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Record that `user_key`'s current structure version is `version`.
    pub fn set(&self, user_key: &[u8], version: u64) {
        self.inner
            .write()
            .expect("version map poisoned")
            .insert(user_key.to_vec(), version);
    }

    /// Forget `user_key` (its subkeys become collectable garbage).
    pub fn remove(&self, user_key: &[u8]) {
        self.inner
            .write()
            .expect("version map poisoned")
            .remove(user_key);
    }

    /// Look up the current version tracked for `user_key`.
    pub fn get(&self, user_key: &[u8]) -> Option<u64> {
        self.inner
            .read()
            .expect("version map poisoned")
            .get(user_key)
            .copied()
    }

    /// Drop every tracked entry (used by `FLUSHDB`-style resets).
    pub fn clear(&self) {
        self.inner.write().expect("version map poisoned").clear();
    }

    /// Number of tracked keys.
    pub fn len(&self) -> usize {
        self.inner.read().expect("version map poisoned").len()
    }

    /// Whether no keys are tracked.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Compaction-filter predicate: should this raw record be kept?
    ///
    /// Metadata records are always kept here (their TTL/version lifecycle is
    /// handled on the foreground path); only stale *versioned* records —
    /// subkeys and ZSet score-index entries, which share the owner+version
    /// key shape — are dropped.
    pub fn should_keep(&self, key: &[u8]) -> bool {
        match encoding::parse_owner_version(key) {
            None => true, // not a versioned record (e.g. metadata) -> keep
            Some((user_key, version)) => match self.get(user_key) {
                Some(current) => version == current,
                None => false, // owner deleted -> record is garbage
            },
        }
    }
}

impl GcPredicate for VersionMap {
    fn should_keep(&self, key: &[u8], _value: &[u8]) -> bool {
        VersionMap::should_keep(self, key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_metadata_records() {
        let vm = VersionMap::new();
        assert!(vm.should_keep(&encoding::meta_key(b"k")));
    }

    #[test]
    fn keeps_current_version_drops_stale_and_orphans() {
        let vm = VersionMap::new();
        vm.set(b"k", 5);

        let current = encoding::subkey(b"k", 5, b"f");
        let stale = encoding::subkey(b"k", 4, b"f");
        let orphan = encoding::subkey(b"other", 1, b"f");

        assert!(vm.should_keep(&current), "current version kept");
        assert!(!vm.should_keep(&stale), "stale version dropped");
        assert!(!vm.should_keep(&orphan), "orphan (unknown owner) dropped");
    }

    #[test]
    fn zscore_index_records_follow_the_same_lifecycle() {
        let vm = VersionMap::new();
        vm.set(b"lb", 5);

        let current = encoding::zscore_key(b"lb", 5, 1.5, b"m");
        let stale = encoding::zscore_key(b"lb", 4, 1.5, b"m");
        let orphan = encoding::zscore_key(b"gone", 1, 1.5, b"m");

        assert!(vm.should_keep(&current), "current score index kept");
        assert!(!vm.should_keep(&stale), "stale score index dropped");
        assert!(!vm.should_keep(&orphan), "orphan score index dropped");
    }

    #[test]
    fn removing_owner_orphans_its_subkeys() {
        let vm = VersionMap::new();
        vm.set(b"k", 9);
        let sk = encoding::subkey(b"k", 9, b"f");
        assert!(vm.should_keep(&sk));
        vm.remove(b"k");
        assert!(!vm.should_keep(&sk));
    }

    #[test]
    fn clone_shares_state() {
        let vm = VersionMap::new();
        let clone = vm.clone();
        vm.set(b"k", 1);
        assert_eq!(clone.get(b"k"), Some(1));
        assert_eq!(clone.len(), 1);
        assert!(!clone.is_empty());
        clone.clear();
        assert!(vm.is_empty());
    }
}
