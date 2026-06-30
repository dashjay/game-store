// Storage layer: Redis data model encoded onto RocksDB.
// Equivalent to spike/rust/src/storage.rs.
#pragma once

#include <cstdint>
#include <memory>
#include <string>
#include <utility>
#include <vector>

#include "gc.h"
#include "rocksdb/db.h"

namespace gamestore {

uint64_t now_ms();

class Store {
 public:
  static std::unique_ptr<Store> Open(const std::string& path);
  ~Store();

  // String
  void Set(const std::string& key, const std::string& value, uint64_t expire_ms);
  bool Get(const std::string& key, std::string* out);

  // Generic
  bool Exists(const std::string& key);
  const char* TypeOf(const std::string& key);
  bool Del(const std::string& key);
  int64_t ExpireMs(const std::string& key, uint64_t expire_at_ms);
  int64_t Pttl(const std::string& key);

  // Hash
  int64_t Hset(const std::string& key,
               const std::vector<std::pair<std::string, std::string>>& pairs);
  bool Hget(const std::string& key, const std::string& field, std::string* out);
  std::vector<std::pair<std::string, std::string>> Hgetall(const std::string& key);
  int64_t Hdel(const std::string& key, const std::vector<std::string>& fields);
  int64_t Hlen(const std::string& key);
  bool Hexists(const std::string& key, const std::string& field);

  // Admin / introspection (spike-only)
  void FlushDb();
  int64_t DbSize();
  int64_t RawSubkeyCount();
  void Compact();

 private:
  Store() = default;
  void RebuildVersionMap();
  bool LoadMeta(const std::string& key, Meta* out);
  bool LoadLiveMeta(const std::string& key, Meta* out);
  void PutMeta(const std::string& key, const Meta& meta);
  void LogicalDelete(const std::string& key);

  rocksdb::DB* db_ = nullptr;
  std::unique_ptr<VersionMap> versions_;
  std::unique_ptr<SubkeyGcFilter> filter_;
};

}  // namespace gamestore
