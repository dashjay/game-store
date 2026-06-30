// On-disk encoding for the general engine layer.
//
// Byte-for-byte identical to the Rust spike (spike/rust/src/encoding.rs) so the
// two implementations are wire- and disk-compatible. Mirrors
// docs/design/03-storage-engine.md §2:
//
//   metadata: [META_PREFIX][user_key]
//   subkey  : [SUBKEY_PREFIX][u32 BE key_len][user_key][u64 BE version][field]
//
//   metadata value: [type:1][version:u64 BE][expire_ms:u64 BE][payload]
//     - String payload = raw value bytes
//     - Hash   payload = [field_count:u32 BE]
#pragma once

#include <cstdint>
#include <optional>
#include <string>

namespace gamestore {

constexpr uint8_t META_PREFIX = 0x01;
constexpr uint8_t SUBKEY_PREFIX = 0x02;

constexpr uint8_t TYPE_STRING = 1;
constexpr uint8_t TYPE_HASH = 2;

constexpr size_t META_HEADER_LEN = 1 + 8 + 8;  // type + version + expire_ms

inline void put_u32_be(std::string& out, uint32_t v) {
  out.push_back(static_cast<char>((v >> 24) & 0xff));
  out.push_back(static_cast<char>((v >> 16) & 0xff));
  out.push_back(static_cast<char>((v >> 8) & 0xff));
  out.push_back(static_cast<char>(v & 0xff));
}

inline void put_u64_be(std::string& out, uint64_t v) {
  for (int shift = 56; shift >= 0; shift -= 8) {
    out.push_back(static_cast<char>((v >> shift) & 0xff));
  }
}

inline uint32_t get_u32_be(const char* p) {
  return (static_cast<uint32_t>(static_cast<uint8_t>(p[0])) << 24) |
         (static_cast<uint32_t>(static_cast<uint8_t>(p[1])) << 16) |
         (static_cast<uint32_t>(static_cast<uint8_t>(p[2])) << 8) |
         (static_cast<uint32_t>(static_cast<uint8_t>(p[3])));
}

inline uint64_t get_u64_be(const char* p) {
  uint64_t v = 0;
  for (int i = 0; i < 8; ++i) {
    v = (v << 8) | static_cast<uint8_t>(p[i]);
  }
  return v;
}

inline std::string meta_key(const std::string& user_key) {
  std::string k;
  k.reserve(1 + user_key.size());
  k.push_back(static_cast<char>(META_PREFIX));
  k.append(user_key);
  return k;
}

inline std::string subkey_prefix(const std::string& user_key, uint64_t version) {
  std::string k;
  k.reserve(1 + 4 + user_key.size() + 8);
  k.push_back(static_cast<char>(SUBKEY_PREFIX));
  put_u32_be(k, static_cast<uint32_t>(user_key.size()));
  k.append(user_key);
  put_u64_be(k, version);
  return k;
}

inline std::string subkey(const std::string& user_key, uint64_t version,
                          const std::string& field) {
  std::string k = subkey_prefix(user_key, version);
  k.append(field);
  return k;
}

// Parsed view of a subkey record key.
struct ParsedSubkey {
  std::string user_key;
  uint64_t version;
  std::string field;
};

inline std::optional<ParsedSubkey> parse_subkey(const char* data, size_t len) {
  if (len < 1 + 4 + 8 || static_cast<uint8_t>(data[0]) != SUBKEY_PREFIX) {
    return std::nullopt;
  }
  uint32_t klen = get_u32_be(data + 1);
  size_t key_start = 5;
  size_t key_end = key_start + klen;
  size_t ver_end = key_end + 8;
  if (len < ver_end) {
    return std::nullopt;
  }
  ParsedSubkey out;
  out.user_key.assign(data + key_start, klen);
  out.version = get_u64_be(data + key_end);
  out.field.assign(data + ver_end, len - ver_end);
  return out;
}

// Decoded metadata record.
struct Meta {
  uint8_t type_id = 0;
  uint64_t version = 0;
  uint64_t expire_ms = 0;
  std::string payload;

  std::string encode() const {
    std::string v;
    v.reserve(META_HEADER_LEN + payload.size());
    v.push_back(static_cast<char>(type_id));
    put_u64_be(v, version);
    put_u64_be(v, expire_ms);
    v.append(payload);
    return v;
  }

  static std::optional<Meta> decode(const std::string& raw) {
    if (raw.size() < META_HEADER_LEN) {
      return std::nullopt;
    }
    Meta m;
    m.type_id = static_cast<uint8_t>(raw[0]);
    m.version = get_u64_be(raw.data() + 1);
    m.expire_ms = get_u64_be(raw.data() + 9);
    m.payload.assign(raw.data() + META_HEADER_LEN, raw.size() - META_HEADER_LEN);
    return m;
  }

  uint32_t field_count() const {
    if (type_id == TYPE_HASH && payload.size() >= 4) {
      return get_u32_be(payload.data());
    }
    return 0;
  }

  void set_field_count(uint32_t n) {
    payload.clear();
    put_u32_be(payload, n);
  }
};

}  // namespace gamestore
