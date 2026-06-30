// Minimal RESP2 wire protocol, equivalent to spike/rust/src/resp.rs.
#pragma once

#include <cstdint>
#include <string>
#include <vector>

namespace gamestore {

// A reply value in the RESP2 type system.
struct Reply {
  enum class Kind { Simple, Error, Int, Bulk, Nil, Array };
  Kind kind;
  std::string str;       // Simple / Error / Bulk
  int64_t integer = 0;   // Int
  std::vector<Reply> arr;  // Array

  static Reply Simple(std::string s) { return {Kind::Simple, std::move(s), 0, {}}; }
  static Reply Error(std::string s) { return {Kind::Error, std::move(s), 0, {}}; }
  static Reply Ok() { return Simple("OK"); }
  static Reply Int(int64_t n) { return {Kind::Int, "", n, {}}; }
  static Reply Bulk(std::string s) { return {Kind::Bulk, std::move(s), 0, {}}; }
  static Reply Nil() { return {Kind::Nil, "", 0, {}}; }
  static Reply Array(std::vector<Reply> items) {
    return {Kind::Array, "", 0, std::move(items)};
  }

  void serialize(std::string* out) const;
};

// Reads one client command from a socket fd into `args`.
// Returns: 1 = got a command, 0 = clean EOF, -1 = error.
class RespReader {
 public:
  explicit RespReader(int fd) : fd_(fd) {}
  int ReadCommand(std::vector<std::string>* args);

 private:
  bool ReadLine(std::string* line);
  bool ReadN(size_t n, std::string* out);
  bool FillBuffer();

  int fd_;
  std::string buf_;
  size_t pos_ = 0;
};

}  // namespace gamestore
