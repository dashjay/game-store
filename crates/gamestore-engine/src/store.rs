//! Redis data model encoded onto a [`GeneralEngine`].
//!
//! Ported and hardened from `spike/rust/src/storage.rs`. Compared to the spike:
//!
//! - it is generic over any [`GeneralEngine`] (RocksDB today, LSH later) rather
//!   than owning a `rocksdb::DB` directly,
//! - every operation returns [`Result`] instead of `.expect()`-panicking,
//! - type mismatches surface a Redis-style [`EngineError::WrongType`] instead of
//!   being silently swallowed,
//! - the version map is wired into the engine's compaction filter via
//!   [`GeneralEngine::install_gc`].
//!
//! It implements the String/Hash operations and the `RAWCOUNT`/`DBSIZE`/
//! `COMPACT` introspection used by the consistency tests (plan I-03). The
//! command layer (I-04, `gamestore-datamodel`) parses Redis commands and calls
//! into these operations.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::encoding::{self, Meta, TYPE_HASH, TYPE_STRING};
use crate::engine::{GeneralEngine, Range, WriteBatch};
use crate::error::{EngineError, Result};
use crate::gc::VersionMap;
use crate::rocks::{EngineConfig, RocksEngine};

/// Current wall-clock time in unix-epoch milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

/// Monotonic, globally increasing structure version. Seeded from wall-clock
/// microseconds so versions keep increasing across restarts and a deleted key
/// can never collide with a freshly rebuilt one (cf. HLC in the design docs).
static VERSION_CLOCK: AtomicU64 = AtomicU64::new(0);

fn next_version() -> u64 {
    loop {
        let prev = VERSION_CLOCK.load(Ordering::SeqCst);
        let candidate = std::cmp::max(prev + 1, now_micros());
        if VERSION_CLOCK
            .compare_exchange(prev, candidate, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return candidate;
        }
    }
}

/// Redis data model over a pluggable [`GeneralEngine`].
pub struct Store<E: GeneralEngine> {
    engine: E,
    versions: VersionMap,
}

impl Store<RocksEngine> {
    /// Open a RocksDB-backed store at `path` with `config`, installing
    /// version-based GC and rebuilding the version map from persisted metadata.
    pub fn open(path: impl AsRef<std::path::Path>, config: &EngineConfig) -> Result<Self> {
        let engine = RocksEngine::open(path, config)?;
        Store::with_engine(engine)
    }
}

impl<E: GeneralEngine> Store<E> {
    /// Wrap an already-opened engine: install the version map as its GC
    /// predicate and rebuild that map from persisted metadata.
    pub fn with_engine(engine: E) -> Result<Self> {
        let versions = VersionMap::new();
        engine.install_gc(Arc::new(versions.clone()));
        let store = Store { engine, versions };
        store.rebuild_version_map()?;
        Ok(store)
    }

    /// Borrow the underlying engine (for benchmarks / advanced callers).
    pub fn engine(&self) -> &E {
        &self.engine
    }

    /// Repopulate the in-memory version map from persisted metadata on startup.
    fn rebuild_version_map(&self) -> Result<()> {
        self.versions.clear();
        for item in self.engine.scan_prefix(&[encoding::META_PREFIX]) {
            let (k, v) = item?;
            // Defensive: scan_prefix already restricts to META_PREFIX.
            if k.first() != Some(&encoding::META_PREFIX) {
                continue;
            }
            if let Some(meta) = Meta::decode(&v) {
                if meta.type_id == TYPE_HASH {
                    let user_key = &k[1..];
                    self.versions.set(user_key, meta.version);
                }
            }
        }
        Ok(())
    }

    fn load_meta(&self, user_key: &[u8]) -> Result<Option<Meta>> {
        match self.engine.get(&encoding::meta_key(user_key))? {
            Some(raw) => Meta::decode(&raw)
                .ok_or_else(|| EngineError::corruption("undecodable metadata record"))
                .map(Some),
            None => Ok(None),
        }
    }

    /// Load metadata, applying lazy expiration: an expired key is treated as
    /// missing and logically deleted (metadata removed, version dropped so its
    /// subkeys become collectable).
    fn load_live_meta(&self, user_key: &[u8]) -> Result<Option<Meta>> {
        let meta = match self.load_meta(user_key)? {
            Some(m) => m,
            None => return Ok(None),
        };
        if meta.expire_ms != 0 && now_ms() >= meta.expire_ms {
            self.logical_delete(user_key)?;
            return Ok(None);
        }
        Ok(Some(meta))
    }

    fn put_meta(&self, user_key: &[u8], meta: &Meta) -> Result<()> {
        let mut batch = WriteBatch::new();
        batch.put(encoding::meta_key(user_key), meta.encode());
        self.engine.write(batch)
    }

    fn logical_delete(&self, user_key: &[u8]) -> Result<()> {
        // Remove metadata + drop from version map. Subkeys (if any) are now
        // orphaned and will be reclaimed by the compaction filter.
        let mut batch = WriteBatch::new();
        batch.delete(encoding::meta_key(user_key));
        self.engine.write(batch)?;
        self.versions.remove(user_key);
        Ok(())
    }

    // ---- String ----------------------------------------------------------

    /// `SET key value [expire_ms]`. Overwrites any previous value/type.
    /// `expire_ms == 0` means no expiry.
    pub fn set(&self, key: &[u8], value: &[u8], expire_ms: u64) -> Result<()> {
        // Overwriting with a String drops any previous (e.g. Hash) structure.
        self.versions.remove(key);
        let meta = Meta {
            type_id: TYPE_STRING,
            version: next_version(),
            expire_ms,
            payload: value.to_vec(),
        };
        self.put_meta(key, &meta)
    }

    /// `GET key`. `Ok(None)` if absent; `WrongType` if the key is not a String.
    pub fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let meta = match self.load_live_meta(key)? {
            Some(m) => m,
            None => return Ok(None),
        };
        if meta.type_id != TYPE_STRING {
            return Err(EngineError::WrongType);
        }
        Ok(Some(meta.payload))
    }

    // ---- Generic ----------------------------------------------------------

    /// Whether `key` exists (and is live).
    pub fn exists(&self, key: &[u8]) -> Result<bool> {
        Ok(self.load_live_meta(key)?.is_some())
    }

    /// Redis `TYPE`: `"string"`, `"hash"`, or `"none"`.
    pub fn type_of(&self, key: &[u8]) -> Result<&'static str> {
        Ok(match self.load_live_meta(key)? {
            Some(m) if m.type_id == TYPE_STRING => "string",
            Some(m) if m.type_id == TYPE_HASH => "hash",
            _ => "none",
        })
    }

    /// `DEL key`. Returns whether a live key was removed.
    pub fn del(&self, key: &[u8]) -> Result<bool> {
        if self.load_live_meta(key)?.is_some() {
            self.logical_delete(key)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Set an absolute expiry (unix-epoch ms). Returns `true` if the key exists.
    pub fn expire_at(&self, key: &[u8], expire_at_ms: u64) -> Result<bool> {
        match self.load_live_meta(key)? {
            Some(mut meta) => {
                meta.expire_ms = expire_at_ms;
                self.put_meta(key, &meta)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Remaining TTL in ms: `-2` if no key, `-1` if no expiry, else remaining.
    pub fn pttl(&self, key: &[u8]) -> Result<i64> {
        Ok(match self.load_live_meta(key)? {
            None => -2,
            Some(meta) => {
                if meta.expire_ms == 0 {
                    -1
                } else {
                    meta.expire_ms.saturating_sub(now_ms()) as i64
                }
            }
        })
    }

    // ---- Hash -------------------------------------------------------------

    /// `HSET key field value [field value ...]`. Returns the number of *new*
    /// fields created. `WrongType` if the key holds a non-Hash value.
    pub fn hset(&self, key: &[u8], pairs: &[(Vec<u8>, Vec<u8>)]) -> Result<i64> {
        let mut meta = match self.load_live_meta(key)? {
            Some(m) if m.type_id == TYPE_HASH => m,
            Some(_) => return Err(EngineError::WrongType),
            None => {
                // New key -> fresh version.
                let version = next_version();
                let mut m = Meta {
                    type_id: TYPE_HASH,
                    version,
                    expire_ms: 0,
                    payload: Vec::new(),
                };
                m.set_field_count(0);
                self.versions.set(key, version);
                m
            }
        };

        let mut field_count = meta.field_count();
        let mut created = 0i64;
        let mut batch = WriteBatch::new();
        for (field, value) in pairs {
            let sk = encoding::subkey(key, meta.version, field);
            let existed = self.engine.get(&sk)?.is_some();
            if !existed {
                field_count += 1;
                created += 1;
            }
            batch.put(sk, value.clone());
        }
        meta.set_field_count(field_count);
        batch.put(encoding::meta_key(key), meta.encode());
        self.engine.write(batch)?;
        Ok(created)
    }

    /// `HGET key field`. `Ok(None)` if absent; `WrongType` if not a Hash.
    pub fn hget(&self, key: &[u8], field: &[u8]) -> Result<Option<Vec<u8>>> {
        let meta = match self.load_live_meta(key)? {
            Some(m) if m.type_id == TYPE_HASH => m,
            Some(_) => return Err(EngineError::WrongType),
            None => return Ok(None),
        };
        let sk = encoding::subkey(key, meta.version, field);
        self.engine.get(&sk)
    }

    /// `HGETALL key`. Empty vec if absent; `WrongType` if not a Hash.
    pub fn hgetall(&self, key: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let meta = match self.load_live_meta(key)? {
            Some(m) if m.type_id == TYPE_HASH => m,
            Some(_) => return Err(EngineError::WrongType),
            None => return Ok(Vec::new()),
        };
        let prefix = encoding::subkey_prefix(key, meta.version);
        let mut out = Vec::new();
        for item in self.engine.scan_prefix(&prefix) {
            let (k, v) = item?;
            let field = k[prefix.len()..].to_vec();
            out.push((field, v));
        }
        Ok(out)
    }

    /// `HDEL key field [field ...]`. Returns number of fields removed.
    /// `WrongType` if the key holds a non-Hash value.
    pub fn hdel(&self, key: &[u8], fields: &[Vec<u8>]) -> Result<i64> {
        let mut meta = match self.load_live_meta(key)? {
            Some(m) if m.type_id == TYPE_HASH => m,
            Some(_) => return Err(EngineError::WrongType),
            None => return Ok(0),
        };
        let mut field_count = meta.field_count();
        let mut removed = 0i64;
        let mut batch = WriteBatch::new();
        for field in fields {
            let sk = encoding::subkey(key, meta.version, field);
            if self.engine.get(&sk)?.is_some() {
                batch.delete(sk);
                removed += 1;
                field_count = field_count.saturating_sub(1);
            }
        }
        if removed > 0 {
            if field_count == 0 {
                // Last field gone -> logical-delete the whole key (subkeys in the
                // batch are still deleted; remaining orphans are GC'd).
                self.engine.write(batch)?;
                self.logical_delete(key)?;
            } else {
                meta.set_field_count(field_count);
                batch.put(encoding::meta_key(key), meta.encode());
                self.engine.write(batch)?;
            }
        }
        Ok(removed)
    }

    /// `HLEN key`. `0` if absent; `WrongType` if not a Hash.
    pub fn hlen(&self, key: &[u8]) -> Result<i64> {
        match self.load_live_meta(key)? {
            Some(m) if m.type_id == TYPE_HASH => Ok(m.field_count() as i64),
            Some(_) => Err(EngineError::WrongType),
            None => Ok(0),
        }
    }

    /// `HEXISTS key field`. `WrongType` if the key holds a non-Hash value.
    pub fn hexists(&self, key: &[u8], field: &[u8]) -> Result<bool> {
        Ok(self.hget(key, field)?.is_some())
    }

    // ---- Admin / introspection -------------------------------------------

    /// Total number of physical metadata records (logical key count) — `DBSIZE`.
    pub fn dbsize(&self) -> Result<i64> {
        let mut n = 0i64;
        for item in self.engine.scan_prefix(&[encoding::META_PREFIX]) {
            item?;
            n += 1;
        }
        Ok(n)
    }

    /// Total number of physical subkey records currently stored — `RAWCOUNT`.
    ///
    /// Used by the consistency tests to prove the compaction filter reclaimed
    /// stale subkeys down to zero.
    pub fn raw_subkey_count(&self) -> Result<i64> {
        let mut n = 0i64;
        for item in self.engine.scan_prefix(&[encoding::SUBKEY_PREFIX]) {
            item?;
            n += 1;
        }
        Ok(n)
    }

    /// Force a full compaction so the compaction filter runs synchronously —
    /// `COMPACT`. After this, stale subkeys are physically gone.
    pub fn compact(&self) -> Result<()> {
        self.engine.compact_range(Some(Range::default()))
    }

    /// `FLUSHDB` / `FLUSHALL`: delete **every** record (metadata and subkeys)
    /// and clear the version map.
    ///
    /// Both record families are removed in one atomic batch so a crash cannot
    /// leave metadata pointing at deleted subkeys (or vice versa). The version
    /// map is cleared afterwards; even if a stale subkey survived (it cannot,
    /// the batch is atomic), an empty version map marks it as orphaned garbage
    /// for the compaction filter.
    pub fn flush_all(&self) -> Result<()> {
        let mut batch = WriteBatch::new();
        for prefix in [encoding::META_PREFIX, encoding::SUBKEY_PREFIX] {
            for item in self.engine.scan_prefix(&[prefix]) {
                let (k, _) = item?;
                batch.delete(k);
            }
        }
        self.engine.write(batch)?;
        self.versions.clear();
        Ok(())
    }
}
