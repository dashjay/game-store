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

use crate::encoding::{self, Meta, TYPE_HASH, TYPE_LIST, TYPE_SET, TYPE_STRING, TYPE_ZSET};
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

/// Decode the score stored in a ZSet member subkey (8 order-preserving bytes).
fn decode_stored_score(raw: &[u8]) -> Result<f64> {
    let bytes: [u8; 8] = raw
        .try_into()
        .map_err(|_| EngineError::corruption("undecodable zset member score"))?;
    Ok(encoding::decode_score(bytes))
}

/// Normalize a Redis inclusive `[start, stop]` rank range (negative indexes
/// count from the end) against a collection of `len` elements. Returns the
/// clamped `(lo, hi)` offsets, or `None` when the range selects nothing.
fn normalize_range(start: i64, stop: i64, len: i64) -> Option<(usize, usize)> {
    if len <= 0 {
        return None;
    }
    let lo = if start < 0 { start + len } else { start }.max(0);
    let hi = if stop < 0 { stop + len } else { stop }.min(len - 1);
    if lo > hi {
        return None;
    }
    Some((lo as usize, hi as usize))
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
                // Every composite type owns versioned subkeys that the GC
                // predicate must recognise as live.
                if matches!(meta.type_id, TYPE_HASH | TYPE_SET | TYPE_ZSET | TYPE_LIST) {
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

    /// Load live metadata expecting `type_id`: `Ok(None)` when the key is
    /// missing, `WrongType` when it holds a different Redis type.
    fn load_typed(&self, user_key: &[u8], type_id: u8) -> Result<Option<Meta>> {
        match self.load_live_meta(user_key)? {
            Some(m) if m.type_id == type_id => Ok(Some(m)),
            Some(_) => Err(EngineError::WrongType),
            None => Ok(None),
        }
    }

    /// Fresh metadata for a new composite key, registering its version in the
    /// GC map so its subkeys are recognised as live.
    fn new_composite_meta(&self, user_key: &[u8], type_id: u8) -> Meta {
        let version = next_version();
        self.versions.set(user_key, version);
        Meta {
            type_id,
            version,
            expire_ms: 0,
            payload: Vec::new(),
        }
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

    /// Redis `TYPE`: `"string"` / `"hash"` / `"set"` / `"zset"` / `"list"`,
    /// or `"none"` for a missing key.
    pub fn type_of(&self, key: &[u8]) -> Result<&'static str> {
        Ok(match self.load_live_meta(key)? {
            Some(m) if m.type_id == TYPE_STRING => "string",
            Some(m) if m.type_id == TYPE_HASH => "hash",
            Some(m) if m.type_id == TYPE_SET => "set",
            Some(m) if m.type_id == TYPE_ZSET => "zset",
            Some(m) if m.type_id == TYPE_LIST => "list",
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
        let mut meta = match self.load_typed(key, TYPE_HASH)? {
            Some(m) => m,
            None => {
                let mut m = self.new_composite_meta(key, TYPE_HASH);
                m.set_field_count(0);
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
        let meta = match self.load_typed(key, TYPE_HASH)? {
            Some(m) => m,
            None => return Ok(None),
        };
        let sk = encoding::subkey(key, meta.version, field);
        self.engine.get(&sk)
    }

    /// `HGETALL key`. Empty vec if absent; `WrongType` if not a Hash.
    pub fn hgetall(&self, key: &[u8]) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let meta = match self.load_typed(key, TYPE_HASH)? {
            Some(m) => m,
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
        let mut meta = match self.load_typed(key, TYPE_HASH)? {
            Some(m) => m,
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
        Ok(self
            .load_typed(key, TYPE_HASH)?
            .map_or(0, |m| m.field_count() as i64))
    }

    /// `HEXISTS key field`. `WrongType` if the key holds a non-Hash value.
    pub fn hexists(&self, key: &[u8], field: &[u8]) -> Result<bool> {
        Ok(self.hget(key, field)?.is_some())
    }

    // ---- Set (I-06) --------------------------------------------------------
    //
    // Encoding (03 §2.3): each member is a subkey with an empty value —
    // membership is expressed by the key itself.

    /// `SADD key member [member ...]`. Returns the number of members actually
    /// added (duplicates within the call and pre-existing members count zero).
    /// `WrongType` if the key holds a non-Set value.
    pub fn sadd(&self, key: &[u8], members: &[Vec<u8>]) -> Result<i64> {
        let mut meta = match self.load_typed(key, TYPE_SET)? {
            Some(m) => m,
            None => {
                let mut m = self.new_composite_meta(key, TYPE_SET);
                m.set_field_count(0);
                m
            }
        };

        let mut count = meta.field_count();
        let mut added = 0i64;
        let mut batch = WriteBatch::new();
        let mut seen: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
        for member in members {
            if !seen.insert(member.as_slice()) {
                continue; // duplicate within this call
            }
            let sk = encoding::subkey(key, meta.version, member);
            if self.engine.get(&sk)?.is_none() {
                count += 1;
                added += 1;
                batch.put(sk, Vec::new());
            }
        }
        if added > 0 {
            meta.set_field_count(count);
            batch.put(encoding::meta_key(key), meta.encode());
            self.engine.write(batch)?;
        }
        Ok(added)
    }

    /// `SREM key member [member ...]`. Returns the number of members removed;
    /// removing the last member deletes the key. `WrongType` on mismatch.
    pub fn srem(&self, key: &[u8], members: &[Vec<u8>]) -> Result<i64> {
        let mut meta = match self.load_typed(key, TYPE_SET)? {
            Some(m) => m,
            None => return Ok(0),
        };
        let mut count = meta.field_count();
        let mut removed = 0i64;
        let mut batch = WriteBatch::new();
        let mut seen: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
        for member in members {
            if !seen.insert(member.as_slice()) {
                continue;
            }
            let sk = encoding::subkey(key, meta.version, member);
            if self.engine.get(&sk)?.is_some() {
                batch.delete(sk);
                removed += 1;
                count = count.saturating_sub(1);
            }
        }
        if removed > 0 {
            if count == 0 {
                self.engine.write(batch)?;
                self.logical_delete(key)?;
            } else {
                meta.set_field_count(count);
                batch.put(encoding::meta_key(key), meta.encode());
                self.engine.write(batch)?;
            }
        }
        Ok(removed)
    }

    /// `SISMEMBER key member`. `false` for a missing key; `WrongType` on mismatch.
    pub fn sismember(&self, key: &[u8], member: &[u8]) -> Result<bool> {
        let meta = match self.load_typed(key, TYPE_SET)? {
            Some(m) => m,
            None => return Ok(false),
        };
        let sk = encoding::subkey(key, meta.version, member);
        Ok(self.engine.get(&sk)?.is_some())
    }

    /// `SMEMBERS key`. Empty vec if absent; `WrongType` on mismatch.
    pub fn smembers(&self, key: &[u8]) -> Result<Vec<Vec<u8>>> {
        let meta = match self.load_typed(key, TYPE_SET)? {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };
        let prefix = encoding::subkey_prefix(key, meta.version);
        let mut out = Vec::new();
        for item in self.engine.scan_prefix(&prefix) {
            let (k, _) = item?;
            out.push(k[prefix.len()..].to_vec());
        }
        Ok(out)
    }

    /// `SCARD key`. `0` if absent; `WrongType` on mismatch.
    pub fn scard(&self, key: &[u8]) -> Result<i64> {
        Ok(self
            .load_typed(key, TYPE_SET)?
            .map_or(0, |m| m.field_count() as i64))
    }

    // ---- ZSet (I-06) -------------------------------------------------------
    //
    // Dual encoding (03 §2.3): the member subkey stores the score (lookup by
    // member, `ZSCORE`), and a score-index record ordered by `(score, member)`
    // supports ordered scans (`ZRANGE`, `ZRANGEBYSCORE`).

    /// `ZADD key score member [score member ...]`. Returns the number of *new*
    /// members added. A member repeated within the call is applied last-wins
    /// and counted once, like Redis. `WrongType` on mismatch.
    pub fn zadd(&self, key: &[u8], pairs: &[(f64, Vec<u8>)]) -> Result<i64> {
        let mut meta = match self.load_typed(key, TYPE_ZSET)? {
            Some(m) => m,
            None => {
                let mut m = self.new_composite_meta(key, TYPE_ZSET);
                m.set_field_count(0);
                m
            }
        };

        // Deduplicate within the call, last score wins.
        let mut effective: Vec<(&[u8], f64)> = Vec::with_capacity(pairs.len());
        for (score, member) in pairs {
            match effective.iter_mut().find(|(m, _)| *m == member.as_slice()) {
                Some(slot) => slot.1 = *score,
                None => effective.push((member.as_slice(), *score)),
            }
        }

        let mut count = meta.field_count();
        let mut added = 0i64;
        let mut batch = WriteBatch::new();
        for (member, score) in effective {
            let member_sk = encoding::subkey(key, meta.version, member);
            match self.engine.get(&member_sk)? {
                Some(old_raw) => {
                    let old_score = decode_stored_score(&old_raw)?;
                    if old_score == score {
                        continue; // no-op update
                    }
                    batch.delete(encoding::zscore_key(key, meta.version, old_score, member));
                }
                None => {
                    count += 1;
                    added += 1;
                }
            }
            batch.put(member_sk, encoding::encode_score(score).to_vec());
            batch.put(
                encoding::zscore_key(key, meta.version, score, member),
                Vec::new(),
            );
        }
        if !batch.is_empty() {
            meta.set_field_count(count);
            batch.put(encoding::meta_key(key), meta.encode());
            self.engine.write(batch)?;
        }
        Ok(added)
    }

    /// `ZSCORE key member`. `Ok(None)` when the key or member is missing;
    /// `WrongType` on mismatch.
    pub fn zscore(&self, key: &[u8], member: &[u8]) -> Result<Option<f64>> {
        let meta = match self.load_typed(key, TYPE_ZSET)? {
            Some(m) => m,
            None => return Ok(None),
        };
        let sk = encoding::subkey(key, meta.version, member);
        match self.engine.get(&sk)? {
            Some(raw) => Ok(Some(decode_stored_score(&raw)?)),
            None => Ok(None),
        }
    }

    /// `ZREM key member [member ...]`. Returns the number removed; removing
    /// the last member deletes the key. `WrongType` on mismatch.
    pub fn zrem(&self, key: &[u8], members: &[Vec<u8>]) -> Result<i64> {
        let mut meta = match self.load_typed(key, TYPE_ZSET)? {
            Some(m) => m,
            None => return Ok(0),
        };
        let mut count = meta.field_count();
        let mut removed = 0i64;
        let mut batch = WriteBatch::new();
        let mut seen: std::collections::HashSet<&[u8]> = std::collections::HashSet::new();
        for member in members {
            if !seen.insert(member.as_slice()) {
                continue;
            }
            let sk = encoding::subkey(key, meta.version, member);
            if let Some(raw) = self.engine.get(&sk)? {
                let score = decode_stored_score(&raw)?;
                batch.delete(sk);
                batch.delete(encoding::zscore_key(key, meta.version, score, member));
                removed += 1;
                count = count.saturating_sub(1);
            }
        }
        if removed > 0 {
            if count == 0 {
                self.engine.write(batch)?;
                self.logical_delete(key)?;
            } else {
                meta.set_field_count(count);
                batch.put(encoding::meta_key(key), meta.encode());
                self.engine.write(batch)?;
            }
        }
        Ok(removed)
    }

    /// `ZCARD key`. `0` if absent; `WrongType` on mismatch.
    pub fn zcard(&self, key: &[u8]) -> Result<i64> {
        Ok(self
            .load_typed(key, TYPE_ZSET)?
            .map_or(0, |m| m.field_count() as i64))
    }

    /// `ZRANGE key start stop` (inclusive rank range, negative indexes count
    /// from the end). Returns `(member, score)` in ascending `(score, member)`
    /// order. Empty vec if absent or the range is empty; `WrongType` on mismatch.
    pub fn zrange(&self, key: &[u8], start: i64, stop: i64) -> Result<Vec<(Vec<u8>, f64)>> {
        let meta = match self.load_typed(key, TYPE_ZSET)? {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };
        let len = meta.field_count() as i64;
        let Some((lo, hi)) = normalize_range(start, stop, len) else {
            return Ok(Vec::new());
        };
        let prefix = encoding::zscore_prefix(key, meta.version);
        let mut out = Vec::with_capacity(hi - lo + 1);
        for (rank, item) in self.engine.scan_prefix(&prefix).enumerate() {
            let (k, _) = item?;
            if rank < lo {
                continue;
            }
            if rank > hi {
                break;
            }
            let (score, member) = encoding::split_score_suffix(&k[prefix.len()..])
                .ok_or_else(|| EngineError::corruption("undecodable zset score index"))?;
            out.push((member.to_vec(), score));
        }
        Ok(out)
    }

    /// `ZRANGEBYSCORE key min max` with optionally exclusive bounds. Returns
    /// `(member, score)` in ascending order. `WrongType` on mismatch.
    pub fn zrange_by_score(
        &self,
        key: &[u8],
        min: f64,
        min_exclusive: bool,
        max: f64,
        max_exclusive: bool,
    ) -> Result<Vec<(Vec<u8>, f64)>> {
        let meta = match self.load_typed(key, TYPE_ZSET)? {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };
        let prefix = encoding::zscore_prefix(key, meta.version);
        let mut out = Vec::new();
        for item in self.engine.scan_prefix(&prefix) {
            let (k, _) = item?;
            let (score, member) = encoding::split_score_suffix(&k[prefix.len()..])
                .ok_or_else(|| EngineError::corruption("undecodable zset score index"))?;
            if score > max || (max_exclusive && score == max) {
                break; // scan is score-ordered: nothing further can match
            }
            if score < min || (min_exclusive && score == min) {
                continue;
            }
            out.push((member.to_vec(), score));
        }
        Ok(out)
    }

    // ---- List (I-06) -------------------------------------------------------
    //
    // Encoding (03 §2.3): each element is a subkey whose field is a fixed-width
    // big-endian index; the metadata payload holds the `[head, tail)` bounds.
    // Pushes/pops only touch the ends, so indexes stay dense.

    /// `LPUSH`/`RPUSH key element [element ...]`. Returns the list length
    /// after the push. `WrongType` on mismatch.
    pub fn push(&self, key: &[u8], values: &[Vec<u8>], left: bool) -> Result<i64> {
        let mut meta = match self.load_typed(key, TYPE_LIST)? {
            Some(m) => m,
            None => {
                let mut m = self.new_composite_meta(key, TYPE_LIST);
                m.set_list_bounds(encoding::LIST_INITIAL_INDEX, encoding::LIST_INITIAL_INDEX);
                m
            }
        };
        let (mut head, mut tail) = meta
            .list_bounds()
            .ok_or_else(|| EngineError::corruption("undecodable list bounds"))?;

        let mut batch = WriteBatch::new();
        for value in values {
            let index = if left {
                head -= 1;
                head
            } else {
                let i = tail;
                tail += 1;
                i
            };
            batch.put(
                encoding::subkey(key, meta.version, &encoding::list_index_field(index)),
                value.clone(),
            );
        }
        meta.set_list_bounds(head, tail);
        batch.put(encoding::meta_key(key), meta.encode());
        self.engine.write(batch)?;
        Ok((tail - head) as i64)
    }

    /// `LPOP`/`RPOP key [count]`: pop up to `count` elements from the chosen
    /// end. Popping the last element deletes the key. Empty vec when the key
    /// is missing; `WrongType` on mismatch.
    pub fn pop(&self, key: &[u8], count: usize, left: bool) -> Result<Vec<Vec<u8>>> {
        let mut meta = match self.load_typed(key, TYPE_LIST)? {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };
        let (mut head, mut tail) = meta
            .list_bounds()
            .ok_or_else(|| EngineError::corruption("undecodable list bounds"))?;

        let mut out = Vec::with_capacity(count.min((tail - head) as usize));
        let mut batch = WriteBatch::new();
        while out.len() < count && head < tail {
            let index = if left {
                let i = head;
                head += 1;
                i
            } else {
                tail -= 1;
                tail
            };
            let sk = encoding::subkey(key, meta.version, &encoding::list_index_field(index));
            let value = self
                .engine
                .get(&sk)?
                .ok_or_else(|| EngineError::corruption("missing list element record"))?;
            batch.delete(sk);
            out.push(value);
        }
        if !out.is_empty() {
            if head == tail {
                self.engine.write(batch)?;
                self.logical_delete(key)?;
            } else {
                meta.set_list_bounds(head, tail);
                batch.put(encoding::meta_key(key), meta.encode());
                self.engine.write(batch)?;
            }
        }
        Ok(out)
    }

    /// `LRANGE key start stop` (inclusive, negative indexes from the end).
    /// Empty vec if absent or the range is empty; `WrongType` on mismatch.
    pub fn lrange(&self, key: &[u8], start: i64, stop: i64) -> Result<Vec<Vec<u8>>> {
        let meta = match self.load_typed(key, TYPE_LIST)? {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };
        let (head, tail) = meta
            .list_bounds()
            .ok_or_else(|| EngineError::corruption("undecodable list bounds"))?;
        let len = (tail - head) as i64;
        let Some((lo, hi)) = normalize_range(start, stop, len) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(hi - lo + 1);
        for offset in lo..=hi {
            let index = head + offset as u64;
            let sk = encoding::subkey(key, meta.version, &encoding::list_index_field(index));
            let value = self
                .engine
                .get(&sk)?
                .ok_or_else(|| EngineError::corruption("missing list element record"))?;
            out.push(value);
        }
        Ok(out)
    }

    /// `LLEN key`. `0` if absent; `WrongType` on mismatch.
    pub fn llen(&self, key: &[u8]) -> Result<i64> {
        Ok(match self.load_typed(key, TYPE_LIST)? {
            Some(m) => m
                .list_bounds()
                .map_or(0, |(head, tail)| (tail - head) as i64),
            None => 0,
        })
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

    /// Total number of physical versioned records currently stored — `RAWCOUNT`.
    /// Counts both subkeys and ZSet score-index records (both are per-member
    /// physical records subject to version GC).
    ///
    /// Used by the consistency tests to prove the compaction filter reclaimed
    /// stale records down to zero.
    pub fn raw_subkey_count(&self) -> Result<i64> {
        let mut n = 0i64;
        for prefix in [encoding::SUBKEY_PREFIX, encoding::ZSCORE_PREFIX] {
            for item in self.engine.scan_prefix(&[prefix]) {
                item?;
                n += 1;
            }
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
        for prefix in [
            encoding::META_PREFIX,
            encoding::SUBKEY_PREFIX,
            encoding::ZSCORE_PREFIX,
        ] {
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
