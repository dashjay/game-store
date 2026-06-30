#include "storage.h"

#include <atomic>
#include <chrono>

#include "rocksdb/iterator.h"
#include "rocksdb/options.h"
#include "rocksdb/write_batch.h"

namespace gamestore {

uint64_t now_ms() {
  using namespace std::chrono;
  return duration_cast<milliseconds>(system_clock::now().time_since_epoch()).count();
}

static uint64_t now_micros() {
  using namespace std::chrono;
  return duration_cast<microseconds>(system_clock::now().time_since_epoch()).count();
}

// Monotonic, globally increasing structure version (cf. HLC in the design).
static std::atomic<uint64_t> g_version_clock{0};

static uint64_t next_version() {
  for (;;) {
    uint64_t prev = g_version_clock.load(std::memory_order_seq_cst);
    uint64_t candidate = std::max(prev + 1, now_micros());
    if (g_version_clock.compare_exchange_strong(prev, candidate)) {
      return candidate;
    }
  }
}

std::unique_ptr<Store> Store::Open(const std::string& path) {
  auto store = std::unique_ptr<Store>(new Store());
  store->versions_ = std::make_unique<VersionMap>();
  store->filter_ = std::make_unique<SubkeyGcFilter>(store->versions_.get());

  rocksdb::Options options;
  options.create_if_missing = true;
  options.compaction_filter = store->filter_.get();

  rocksdb::DB* db = nullptr;
  rocksdb::Status s = rocksdb::DB::Open(options, path, &db);
  if (!s.ok()) {
    fprintf(stderr, "[cpp] failed to open RocksDB at %s: %s\n", path.c_str(),
            s.ToString().c_str());
    return nullptr;
  }
  store->db_ = db;
  store->RebuildVersionMap();
  return store;
}

Store::~Store() {
  delete db_;
}

void Store::RebuildVersionMap() {
  std::unique_ptr<rocksdb::Iterator> it(db_->NewIterator(rocksdb::ReadOptions()));
  for (it->SeekToFirst(); it->Valid(); it->Next()) {
    const rocksdb::Slice k = it->key();
    if (k.size() >= 1 && static_cast<uint8_t>(k.data()[0]) == META_PREFIX) {
      auto meta = Meta::decode(it->value().ToString());
      if (meta && meta->type_id == TYPE_HASH) {
        versions_->set(std::string(k.data() + 1, k.size() - 1), meta->version);
      }
    }
  }
}

bool Store::LoadMeta(const std::string& key, Meta* out) {
  std::string raw;
  rocksdb::Status s = db_->Get(rocksdb::ReadOptions(), meta_key(key), &raw);
  if (!s.ok()) {
    return false;
  }
  auto meta = Meta::decode(raw);
  if (!meta) {
    return false;
  }
  *out = std::move(*meta);
  return true;
}

bool Store::LoadLiveMeta(const std::string& key, Meta* out) {
  Meta meta;
  if (!LoadMeta(key, &meta)) {
    return false;
  }
  if (meta.expire_ms != 0 && now_ms() >= meta.expire_ms) {
    LogicalDelete(key);
    return false;
  }
  *out = std::move(meta);
  return true;
}

void Store::PutMeta(const std::string& key, const Meta& meta) {
  db_->Put(rocksdb::WriteOptions(), meta_key(key), meta.encode());
}

void Store::LogicalDelete(const std::string& key) {
  db_->Delete(rocksdb::WriteOptions(), meta_key(key));
  versions_->remove(key);
}

// ---- String ----------------------------------------------------------------

void Store::Set(const std::string& key, const std::string& value, uint64_t expire_ms) {
  versions_->remove(key);  // overwriting drops any prior structure
  Meta meta;
  meta.type_id = TYPE_STRING;
  meta.version = next_version();
  meta.expire_ms = expire_ms;
  meta.payload = value;
  PutMeta(key, meta);
}

bool Store::Get(const std::string& key, std::string* out) {
  Meta meta;
  if (!LoadLiveMeta(key, &meta) || meta.type_id != TYPE_STRING) {
    return false;
  }
  *out = meta.payload;
  return true;
}

// ---- Generic ----------------------------------------------------------------

bool Store::Exists(const std::string& key) {
  Meta meta;
  return LoadLiveMeta(key, &meta);
}

const char* Store::TypeOf(const std::string& key) {
  Meta meta;
  if (!LoadLiveMeta(key, &meta)) {
    return "none";
  }
  if (meta.type_id == TYPE_STRING) return "string";
  if (meta.type_id == TYPE_HASH) return "hash";
  return "none";
}

bool Store::Del(const std::string& key) {
  Meta meta;
  if (!LoadLiveMeta(key, &meta)) {
    return false;
  }
  LogicalDelete(key);
  return true;
}

int64_t Store::ExpireMs(const std::string& key, uint64_t expire_at_ms) {
  Meta meta;
  if (!LoadLiveMeta(key, &meta)) {
    return 0;
  }
  meta.expire_ms = expire_at_ms;
  PutMeta(key, meta);
  return 1;
}

int64_t Store::Pttl(const std::string& key) {
  Meta meta;
  if (!LoadLiveMeta(key, &meta)) {
    return -2;
  }
  if (meta.expire_ms == 0) {
    return -1;
  }
  uint64_t now = now_ms();
  return meta.expire_ms > now ? static_cast<int64_t>(meta.expire_ms - now) : 0;
}

// ---- Hash -------------------------------------------------------------------

int64_t Store::Hset(const std::string& key,
                    const std::vector<std::pair<std::string, std::string>>& pairs) {
  Meta meta;
  bool have = LoadLiveMeta(key, &meta) && meta.type_id == TYPE_HASH;
  if (!have) {
    meta = Meta();
    meta.type_id = TYPE_HASH;
    meta.version = next_version();
    meta.expire_ms = 0;
    meta.set_field_count(0);
    versions_->set(key, meta.version);
  }

  uint32_t field_count = meta.field_count();
  int64_t created = 0;
  rocksdb::WriteBatch batch;
  for (const auto& [field, value] : pairs) {
    std::string sk = subkey(key, meta.version, field);
    std::string existing;
    bool existed = db_->Get(rocksdb::ReadOptions(), sk, &existing).ok();
    if (!existed) {
      field_count += 1;
      created += 1;
    }
    batch.Put(sk, value);
  }
  meta.set_field_count(field_count);
  batch.Put(meta_key(key), meta.encode());
  db_->Write(rocksdb::WriteOptions(), &batch);
  return created;
}

bool Store::Hget(const std::string& key, const std::string& field, std::string* out) {
  Meta meta;
  if (!LoadLiveMeta(key, &meta) || meta.type_id != TYPE_HASH) {
    return false;
  }
  return db_->Get(rocksdb::ReadOptions(), subkey(key, meta.version, field), out).ok();
}

std::vector<std::pair<std::string, std::string>> Store::Hgetall(const std::string& key) {
  std::vector<std::pair<std::string, std::string>> out;
  Meta meta;
  if (!LoadLiveMeta(key, &meta) || meta.type_id != TYPE_HASH) {
    return out;
  }
  std::string prefix = subkey_prefix(key, meta.version);
  std::unique_ptr<rocksdb::Iterator> it(db_->NewIterator(rocksdb::ReadOptions()));
  for (it->Seek(prefix); it->Valid(); it->Next()) {
    const rocksdb::Slice k = it->key();
    if (k.size() < prefix.size() || memcmp(k.data(), prefix.data(), prefix.size()) != 0) {
      break;
    }
    std::string field(k.data() + prefix.size(), k.size() - prefix.size());
    out.emplace_back(std::move(field), it->value().ToString());
  }
  return out;
}

int64_t Store::Hdel(const std::string& key, const std::vector<std::string>& fields) {
  Meta meta;
  if (!LoadLiveMeta(key, &meta) || meta.type_id != TYPE_HASH) {
    return 0;
  }
  uint32_t field_count = meta.field_count();
  int64_t removed = 0;
  rocksdb::WriteBatch batch;
  for (const auto& field : fields) {
    std::string sk = subkey(key, meta.version, field);
    std::string existing;
    if (db_->Get(rocksdb::ReadOptions(), sk, &existing).ok()) {
      batch.Delete(sk);
      removed += 1;
      if (field_count > 0) field_count -= 1;
    }
  }
  if (removed > 0) {
    if (field_count == 0) {
      LogicalDelete(key);
      db_->Write(rocksdb::WriteOptions(), &batch);
    } else {
      meta.set_field_count(field_count);
      batch.Put(meta_key(key), meta.encode());
      db_->Write(rocksdb::WriteOptions(), &batch);
    }
  }
  return removed;
}

int64_t Store::Hlen(const std::string& key) {
  Meta meta;
  if (!LoadLiveMeta(key, &meta) || meta.type_id != TYPE_HASH) {
    return 0;
  }
  return meta.field_count();
}

bool Store::Hexists(const std::string& key, const std::string& field) {
  std::string tmp;
  return Hget(key, field, &tmp);
}

// ---- Admin / introspection --------------------------------------------------

void Store::FlushDb() {
  rocksdb::WriteBatch batch;
  std::unique_ptr<rocksdb::Iterator> it(db_->NewIterator(rocksdb::ReadOptions()));
  for (it->SeekToFirst(); it->Valid(); it->Next()) {
    batch.Delete(it->key());
  }
  db_->Write(rocksdb::WriteOptions(), &batch);
  versions_->clear();
}

int64_t Store::DbSize() {
  int64_t n = 0;
  std::unique_ptr<rocksdb::Iterator> it(db_->NewIterator(rocksdb::ReadOptions()));
  for (it->SeekToFirst(); it->Valid(); it->Next()) {
    const rocksdb::Slice k = it->key();
    if (k.size() >= 1 && static_cast<uint8_t>(k.data()[0]) == META_PREFIX) {
      n += 1;
    }
  }
  return n;
}

int64_t Store::RawSubkeyCount() {
  int64_t n = 0;
  std::unique_ptr<rocksdb::Iterator> it(db_->NewIterator(rocksdb::ReadOptions()));
  for (it->SeekToFirst(); it->Valid(); it->Next()) {
    const rocksdb::Slice k = it->key();
    if (k.size() >= 1 && static_cast<uint8_t>(k.data()[0]) == SUBKEY_PREFIX) {
      n += 1;
    }
  }
  return n;
}

void Store::Compact() {
  // Flush first (the filter only sees SST data, not the memtable) and force the
  // bottommost level so RocksDB cannot skip the rewrite via trivial move.
  db_->Flush(rocksdb::FlushOptions());
  rocksdb::CompactRangeOptions cro;
  cro.bottommost_level_compaction = rocksdb::BottommostLevelCompaction::kForce;
  db_->CompactRange(cro, nullptr, nullptr);
}

}  // namespace gamestore
