//! Storage layer: Redis data model encoded onto RocksDB (the "general engine
//! layer" in `docs/design/03-storage-engine.md`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rocksdb::{
    compaction_filter::Decision, DBCompactionStyle, IteratorMode, Options, WriteBatch, DB,
};

use crate::encoding::{self, Meta, TYPE_HASH, TYPE_STRING};
use crate::gc::VersionMap;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn now_micros() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_micros() as u64
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

pub struct Store {
    db: DB,
    versions: VersionMap,
}

impl Store {
    pub fn open(path: &str) -> Result<Store, String> {
        let versions = VersionMap::new();

        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.set_compaction_style(DBCompactionStyle::Level);
        // Install the version-based subkey GC as a compaction filter.
        let vm = versions.clone();
        opts.set_compaction_filter("gamestore-subkey-gc", move |_level, key, _value| {
            if vm.should_keep(key) {
                Decision::Keep
            } else {
                Decision::Remove
            }
        });

        let db = DB::open(&opts, path).map_err(|e| e.to_string())?;
        let store = Store { db, versions };
        store.rebuild_version_map();
        Ok(store)
    }

    /// Repopulate the in-memory version map from persisted metadata on startup.
    fn rebuild_version_map(&self) {
        let iter = self.db.iterator(IteratorMode::Start);
        for item in iter {
            let (k, v) = match item {
                Ok(kv) => kv,
                Err(_) => continue,
            };
            if k.first() == Some(&encoding::META_PREFIX) {
                if let Some(meta) = Meta::decode(&v) {
                    if meta.type_id == TYPE_HASH {
                        let user_key = &k[1..];
                        self.versions.set(user_key, meta.version);
                    }
                }
            }
        }
    }

    fn load_meta(&self, user_key: &[u8]) -> Option<Meta> {
        let raw = self.db.get(encoding::meta_key(user_key)).ok()??;
        Meta::decode(&raw)
    }

    /// Load metadata, applying lazy expiration: an expired key is treated as
    /// missing and physically logical-deleted (version dropped from the map so
    /// its subkeys become collectable).
    fn load_live_meta(&self, user_key: &[u8]) -> Option<Meta> {
        let meta = self.load_meta(user_key)?;
        if meta.expire_ms != 0 && now_ms() >= meta.expire_ms {
            self.logical_delete(user_key);
            return None;
        }
        Some(meta)
    }

    fn put_meta(&self, user_key: &[u8], meta: &Meta) {
        self.db
            .put(encoding::meta_key(user_key), meta.encode())
            .expect("put meta");
    }

    fn logical_delete(&self, user_key: &[u8]) {
        // Remove metadata + drop from version map. Subkeys (if any) are now
        // orphaned and will be reclaimed by the compaction filter.
        let _ = self.db.delete(encoding::meta_key(user_key));
        self.versions.remove(user_key);
    }

    // ---- String ----------------------------------------------------------

    pub fn set(&self, key: &[u8], value: &[u8], expire_ms: u64) {
        // Overwriting with a String drops any previous (e.g. Hash) structure.
        self.versions.remove(key);
        let meta = Meta {
            type_id: TYPE_STRING,
            version: next_version(),
            expire_ms,
            payload: value.to_vec(),
        };
        self.put_meta(key, &meta);
    }

    pub fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let meta = self.load_live_meta(key)?;
        if meta.type_id != TYPE_STRING {
            return None;
        }
        Some(meta.payload)
    }

    // ---- Generic ----------------------------------------------------------

    pub fn exists(&self, key: &[u8]) -> bool {
        self.load_live_meta(key).is_some()
    }

    pub fn type_of(&self, key: &[u8]) -> &'static str {
        match self.load_live_meta(key) {
            Some(m) if m.type_id == TYPE_STRING => "string",
            Some(m) if m.type_id == TYPE_HASH => "hash",
            _ => "none",
        }
    }

    pub fn del(&self, key: &[u8]) -> bool {
        if self.load_live_meta(key).is_some() {
            self.logical_delete(key);
            true
        } else {
            false
        }
    }

    /// Returns 1 if a timeout was set, 0 if the key does not exist.
    pub fn expire_ms(&self, key: &[u8], expire_at_ms: u64) -> i64 {
        match self.load_live_meta(key) {
            Some(mut meta) => {
                meta.expire_ms = expire_at_ms;
                self.put_meta(key, &meta);
                1
            }
            None => 0,
        }
    }

    /// -2 if no key, -1 if no expire, else remaining milliseconds.
    pub fn pttl(&self, key: &[u8]) -> i64 {
        match self.load_live_meta(key) {
            None => -2,
            Some(meta) => {
                if meta.expire_ms == 0 {
                    -1
                } else {
                    (meta.expire_ms.saturating_sub(now_ms())) as i64
                }
            }
        }
    }

    // ---- Hash -------------------------------------------------------------

    /// Returns the number of *new* fields created.
    pub fn hset(&self, key: &[u8], pairs: &[(Vec<u8>, Vec<u8>)]) -> i64 {
        let mut meta = match self.load_live_meta(key) {
            Some(m) if m.type_id == TYPE_HASH => m,
            _ => {
                // New (or replacing a non-hash) -> fresh version.
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
        let mut batch = WriteBatch::default();
        for (field, value) in pairs {
            let sk = encoding::subkey(key, meta.version, field);
            let existed = self.db.get(&sk).ok().flatten().is_some();
            if !existed {
                field_count += 1;
                created += 1;
            }
            batch.put(&sk, value);
        }
        meta.set_field_count(field_count);
        batch.put(encoding::meta_key(key), meta.encode());
        self.db.write(batch).expect("hset batch");
        created
    }

    pub fn hget(&self, key: &[u8], field: &[u8]) -> Option<Vec<u8>> {
        let meta = self.load_live_meta(key)?;
        if meta.type_id != TYPE_HASH {
            return None;
        }
        let sk = encoding::subkey(key, meta.version, field);
        self.db.get(sk).ok().flatten()
    }

    pub fn hgetall(&self, key: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
        let meta = match self.load_live_meta(key) {
            Some(m) if m.type_id == TYPE_HASH => m,
            _ => return Vec::new(),
        };
        let prefix = encoding::subkey_prefix(key, meta.version);
        let mut out = Vec::new();
        let iter = self
            .db
            .iterator(IteratorMode::From(&prefix, rocksdb::Direction::Forward));
        for item in iter {
            let (k, v) = match item {
                Ok(kv) => kv,
                Err(_) => break,
            };
            if !k.starts_with(&prefix) {
                break;
            }
            let field = k[prefix.len()..].to_vec();
            out.push((field, v.to_vec()));
        }
        out
    }

    pub fn hdel(&self, key: &[u8], fields: &[Vec<u8>]) -> i64 {
        let mut meta = match self.load_live_meta(key) {
            Some(m) if m.type_id == TYPE_HASH => m,
            _ => return 0,
        };
        let mut field_count = meta.field_count();
        let mut removed = 0i64;
        let mut batch = WriteBatch::default();
        for field in fields {
            let sk = encoding::subkey(key, meta.version, field);
            if self.db.get(&sk).ok().flatten().is_some() {
                batch.delete(&sk);
                removed += 1;
                field_count = field_count.saturating_sub(1);
            }
        }
        if removed > 0 {
            if field_count == 0 {
                self.logical_delete(key);
                self.db.write(batch).expect("hdel batch");
            } else {
                meta.set_field_count(field_count);
                batch.put(encoding::meta_key(key), meta.encode());
                self.db.write(batch).expect("hdel batch");
            }
        }
        removed
    }

    pub fn hlen(&self, key: &[u8]) -> i64 {
        match self.load_live_meta(key) {
            Some(m) if m.type_id == TYPE_HASH => m.field_count() as i64,
            _ => 0,
        }
    }

    pub fn hexists(&self, key: &[u8], field: &[u8]) -> bool {
        self.hget(key, field).is_some()
    }

    // ---- Admin / introspection (spike-only) -------------------------------

    pub fn flushdb(&self) {
        let keys: Vec<Box<[u8]>> = self
            .db
            .iterator(IteratorMode::Start)
            .filter_map(|item| item.ok().map(|(k, _)| k))
            .collect();
        let mut batch = WriteBatch::default();
        for k in &keys {
            batch.delete(k);
        }
        self.db.write(batch).expect("flush batch");
        self.versions_clear();
    }

    fn versions_clear(&self) {
        // Rebuild as empty by removing every tracked key.
        // Cheaper: just drop entries we know about via a fresh scan.
        let keys: Vec<Box<[u8]>> = self
            .db
            .iterator(IteratorMode::Start)
            .filter_map(|item| item.ok().map(|(k, _)| k))
            .collect();
        for k in keys {
            if k.first() == Some(&encoding::META_PREFIX) {
                self.versions.remove(&k[1..]);
            }
        }
    }

    /// Total number of physical metadata records (logical key count).
    pub fn dbsize(&self) -> i64 {
        self.db
            .iterator(IteratorMode::Start)
            .filter_map(|i| i.ok())
            .filter(|(k, _)| k.first() == Some(&encoding::META_PREFIX))
            .count() as i64
    }

    /// Total number of physical subkey records currently stored. Used by the
    /// functional test to prove the compaction filter reclaimed stale subkeys.
    pub fn raw_subkey_count(&self) -> i64 {
        self.db
            .iterator(IteratorMode::Start)
            .filter_map(|i| i.ok())
            .filter(|(k, _)| k.first() == Some(&encoding::SUBKEY_PREFIX))
            .count() as i64
    }

    /// Force a full compaction so the compaction filter runs synchronously.
    /// Flush first (the filter only sees SST data, not the memtable) and force
    /// the bottommost level so RocksDB cannot skip the rewrite via trivial move.
    pub fn compact(&self) {
        let _ = self.db.flush();
        let mut copts = rocksdb::CompactOptions::default();
        copts.set_bottommost_level_compaction(rocksdb::BottommostLevelCompaction::Force);
        self.db
            .compact_range_opt::<&[u8], &[u8]>(None, None, &copts);
    }
}
