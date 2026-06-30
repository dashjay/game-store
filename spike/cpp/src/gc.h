// Version-based subkey GC, equivalent to spike/rust/src/gc.rs.
//
// A small in-memory map of user_key -> current structure version. A RocksDB
// CompactionFilter consults it to drop stale subkeys (version != current) and
// orphaned subkeys (owner deleted) in the background, exactly as described in
// docs/design/03-storage-engine.md §4.
#pragma once

#include <mutex>
#include <string>
#include <unordered_map>

#include "encoding.h"
#include "rocksdb/compaction_filter.h"
#include "rocksdb/slice.h"

namespace gamestore {

class VersionMap {
 public:
  void set(const std::string& user_key, uint64_t version) {
    std::lock_guard<std::mutex> lock(mu_);
    map_[user_key] = version;
  }

  void remove(const std::string& user_key) {
    std::lock_guard<std::mutex> lock(mu_);
    map_.erase(user_key);
  }

  bool get(const std::string& user_key, uint64_t* out) const {
    std::lock_guard<std::mutex> lock(mu_);
    auto it = map_.find(user_key);
    if (it == map_.end()) {
      return false;
    }
    *out = it->second;
    return true;
  }

  void clear() {
    std::lock_guard<std::mutex> lock(mu_);
    map_.clear();
  }

  // Keep metadata (and any non-subkey) records; for a subkey, keep iff its
  // owner still exists and the subkey's version matches the current version.
  bool should_keep(const char* key, size_t len) const {
    auto parsed = parse_subkey(key, len);
    if (!parsed) {
      return true;
    }
    uint64_t current = 0;
    if (!get(parsed->user_key, &current)) {
      return false;  // owner deleted -> garbage
    }
    return parsed->version == current;
  }

 private:
  mutable std::mutex mu_;
  std::unordered_map<std::string, uint64_t> map_;
};

// RocksDB compaction filter that removes stale/orphaned subkeys.
class SubkeyGcFilter : public rocksdb::CompactionFilter {
 public:
  explicit SubkeyGcFilter(const VersionMap* versions) : versions_(versions) {}

  bool Filter(int /*level*/, const rocksdb::Slice& key,
              const rocksdb::Slice& /*existing_value*/,
              std::string* /*new_value*/,
              bool* /*value_changed*/) const override {
    // Return true == drop the record.
    return !versions_->should_keep(key.data(), key.size());
  }

  const char* Name() const override { return "gamestore-subkey-gc"; }

 private:
  const VersionMap* versions_;
};

}  // namespace gamestore
