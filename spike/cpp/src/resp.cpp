#include "resp.h"

#include <unistd.h>

#include <cstdlib>

namespace gamestore {

void Reply::serialize(std::string* out) const {
  switch (kind) {
    case Kind::Simple:
      out->push_back('+');
      out->append(str);
      out->append("\r\n");
      break;
    case Kind::Error:
      out->push_back('-');
      out->append(str);
      out->append("\r\n");
      break;
    case Kind::Int:
      out->push_back(':');
      out->append(std::to_string(integer));
      out->append("\r\n");
      break;
    case Kind::Nil:
      out->append("$-1\r\n");
      break;
    case Kind::Bulk:
      out->push_back('$');
      out->append(std::to_string(str.size()));
      out->append("\r\n");
      out->append(str);
      out->append("\r\n");
      break;
    case Kind::Array:
      out->push_back('*');
      out->append(std::to_string(arr.size()));
      out->append("\r\n");
      for (const auto& item : arr) {
        item.serialize(out);
      }
      break;
  }
}

bool RespReader::FillBuffer() {
  char tmp[8192];
  ssize_t n = ::read(fd_, tmp, sizeof(tmp));
  if (n <= 0) {
    return false;
  }
  // Compact consumed bytes occasionally to keep the buffer small.
  if (pos_ > 0 && pos_ == buf_.size()) {
    buf_.clear();
    pos_ = 0;
  }
  buf_.append(tmp, n);
  return true;
}

bool RespReader::ReadLine(std::string* line) {
  line->clear();
  for (;;) {
    while (pos_ < buf_.size()) {
      char c = buf_[pos_++];
      if (c == '\n') {
        if (!line->empty() && line->back() == '\r') {
          line->pop_back();
        }
        return true;
      }
      line->push_back(c);
    }
    if (!FillBuffer()) {
      return false;
    }
  }
}

bool RespReader::ReadN(size_t n, std::string* out) {
  out->clear();
  while (out->size() < n) {
    if (pos_ >= buf_.size() && !FillBuffer()) {
      return false;
    }
    size_t avail = buf_.size() - pos_;
    size_t need = n - out->size();
    size_t take = avail < need ? avail : need;
    out->append(buf_.data() + pos_, take);
    pos_ += take;
  }
  return true;
}

int RespReader::ReadCommand(std::vector<std::string>* args) {
  args->clear();
  std::string line;
  if (!ReadLine(&line)) {
    return 0;  // EOF
  }
  if (line.empty()) {
    return 1;  // empty command, caller skips
  }

  if (line[0] == '*') {
    long count = std::strtol(line.c_str() + 1, nullptr, 10);
    if (count <= 0) {
      return 1;
    }
    for (long i = 0; i < count; ++i) {
      std::string hdr;
      if (!ReadLine(&hdr)) {
        return 0;
      }
      if (hdr.empty() || hdr[0] != '$') {
        return -1;
      }
      long len = std::strtol(hdr.c_str() + 1, nullptr, 10);
      if (len < 0) {
        args->emplace_back();
        continue;
      }
      std::string value;
      if (!ReadN(static_cast<size_t>(len), &value)) {
        return 0;
      }
      std::string crlf;
      if (!ReadN(2, &crlf)) {  // trailing CRLF
        return 0;
      }
      args->push_back(std::move(value));
    }
    return 1;
  }

  // Inline command: split on whitespace.
  std::string token;
  for (char c : line) {
    if (c == ' ' || c == '\t') {
      if (!token.empty()) {
        args->push_back(token);
        token.clear();
      }
    } else {
      token.push_back(c);
    }
  }
  if (!token.empty()) {
    args->push_back(token);
  }
  return 1;
}

}  // namespace gamestore
