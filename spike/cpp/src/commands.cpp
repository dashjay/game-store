#include "commands.h"

#include <algorithm>
#include <cctype>
#include <cstdlib>

namespace gamestore {

static std::string Upper(const std::string& s) {
  std::string out = s;
  std::transform(out.begin(), out.end(), out.begin(),
                 [](unsigned char c) { return std::toupper(c); });
  return out;
}

static std::string Lower(const std::string& s) {
  std::string out = s;
  std::transform(out.begin(), out.end(), out.begin(),
                 [](unsigned char c) { return std::tolower(c); });
  return out;
}

static Reply WrongArgs(const std::string& cmd) {
  return Reply::Error("ERR wrong number of arguments for '" + Lower(cmd) +
                      "' command");
}

static bool ParseU64(const std::string& s, uint64_t* out) {
  if (s.empty()) return false;
  char* end = nullptr;
  unsigned long long v = std::strtoull(s.c_str(), &end, 10);
  if (end == s.c_str() || *end != '\0') return false;
  *out = static_cast<uint64_t>(v);
  return true;
}

static Reply CmdSet(Store* store, const std::vector<std::string>& a) {
  if (a.size() < 2) return WrongArgs("set");
  uint64_t expire_ms = 0;
  for (size_t i = 2; i < a.size();) {
    std::string opt = Upper(a[i]);
    if (opt == "EX" || opt == "PX") {
      if (i + 1 >= a.size()) return Reply::Error("ERR syntax error");
      uint64_t n;
      if (!ParseU64(a[i + 1], &n)) {
        return Reply::Error("ERR value is not an integer or out of range");
      }
      expire_ms = now_ms() + (opt == "EX" ? n * 1000 : n);
      i += 2;
    } else {
      return Reply::Error("ERR syntax error");
    }
  }
  store->Set(a[0], a[1], expire_ms);
  return Reply::Ok();
}

static Reply CmdExpire(Store* store, const std::vector<std::string>& a,
                       uint64_t unit_ms) {
  if (a.size() != 2) return WrongArgs("expire");
  uint64_t n;
  if (!ParseU64(a[1], &n)) {
    return Reply::Error("ERR value is not an integer or out of range");
  }
  return Reply::Int(store->ExpireMs(a[0], now_ms() + n * unit_ms));
}

static Reply CmdTtl(Store* store, const std::vector<std::string>& a, bool seconds) {
  if (a.size() != 1) return WrongArgs("ttl");
  int64_t pttl = store->Pttl(a[0]);
  if (pttl < 0) return Reply::Int(pttl);
  return Reply::Int(seconds ? (pttl + 999) / 1000 : pttl);
}

static Reply CmdHset(Store* store, const std::vector<std::string>& a,
                     const std::string& cmd) {
  if (a.size() < 3 || (a.size() - 1) % 2 != 0) return WrongArgs("hset");
  std::vector<std::pair<std::string, std::string>> pairs;
  for (size_t i = 1; i + 1 < a.size(); i += 2) {
    pairs.emplace_back(a[i], a[i + 1]);
  }
  int64_t created = store->Hset(a[0], pairs);
  if (cmd == "HMSET") return Reply::Ok();
  return Reply::Int(created);
}

Reply Dispatch(Store* store, const std::vector<std::string>& args, bool* close) {
  *close = false;
  if (args.empty()) {
    return Reply::Error("ERR empty command");
  }
  std::string cmd = Upper(args[0]);
  std::vector<std::string> a(args.begin() + 1, args.end());

  if (cmd == "PING") {
    return a.empty() ? Reply::Simple("PONG") : Reply::Bulk(a[0]);
  }
  if (cmd == "ECHO") {
    return a.size() == 1 ? Reply::Bulk(a[0]) : WrongArgs("echo");
  }
  if (cmd == "CLIENT" || cmd == "SELECT" || cmd == "HELLO") {
    return Reply::Ok();
  }
  if (cmd == "COMMAND") {
    return Reply::Array({});
  }
  if (cmd == "QUIT") {
    *close = true;
    return Reply::Ok();
  }

  if (cmd == "SET") return CmdSet(store, a);
  if (cmd == "GET") {
    if (a.size() != 1) return WrongArgs("get");
    std::string v;
    return store->Get(a[0], &v) ? Reply::Bulk(v) : Reply::Nil();
  }
  if (cmd == "DEL") {
    if (a.empty()) return WrongArgs("del");
    int64_t n = 0;
    for (const auto& k : a) {
      if (store->Del(k)) n++;
    }
    return Reply::Int(n);
  }
  if (cmd == "EXISTS") {
    if (a.empty()) return WrongArgs("exists");
    int64_t n = 0;
    for (const auto& k : a) {
      if (store->Exists(k)) n++;
    }
    return Reply::Int(n);
  }
  if (cmd == "TYPE") {
    if (a.size() != 1) return WrongArgs("type");
    return Reply::Simple(store->TypeOf(a[0]));
  }
  if (cmd == "EXPIRE") return CmdExpire(store, a, 1000);
  if (cmd == "PEXPIRE") return CmdExpire(store, a, 1);
  if (cmd == "TTL") return CmdTtl(store, a, true);
  if (cmd == "PTTL") return CmdTtl(store, a, false);

  if (cmd == "HSET" || cmd == "HMSET") return CmdHset(store, a, cmd);
  if (cmd == "HGET") {
    if (a.size() != 2) return WrongArgs("hget");
    std::string v;
    return store->Hget(a[0], a[1], &v) ? Reply::Bulk(v) : Reply::Nil();
  }
  if (cmd == "HMGET") {
    if (a.size() < 2) return WrongArgs("hmget");
    std::vector<Reply> items;
    for (size_t i = 1; i < a.size(); ++i) {
      std::string v;
      items.push_back(store->Hget(a[0], a[i], &v) ? Reply::Bulk(v) : Reply::Nil());
    }
    return Reply::Array(std::move(items));
  }
  if (cmd == "HGETALL") {
    if (a.size() != 1) return WrongArgs("hgetall");
    std::vector<Reply> items;
    for (auto& [f, v] : store->Hgetall(a[0])) {
      items.push_back(Reply::Bulk(f));
      items.push_back(Reply::Bulk(v));
    }
    return Reply::Array(std::move(items));
  }
  if (cmd == "HDEL") {
    if (a.size() < 2) return WrongArgs("hdel");
    std::vector<std::string> fields(a.begin() + 1, a.end());
    return Reply::Int(store->Hdel(a[0], fields));
  }
  if (cmd == "HLEN") {
    if (a.size() != 1) return WrongArgs("hlen");
    return Reply::Int(store->Hlen(a[0]));
  }
  if (cmd == "HEXISTS") {
    if (a.size() != 2) return WrongArgs("hexists");
    return Reply::Int(store->Hexists(a[0], a[1]) ? 1 : 0);
  }

  if (cmd == "FLUSHDB" || cmd == "FLUSHALL") {
    store->FlushDb();
    return Reply::Ok();
  }
  if (cmd == "DBSIZE") return Reply::Int(store->DbSize());
  if (cmd == "COMPACT") {
    store->Compact();
    return Reply::Ok();
  }
  if (cmd == "RAWCOUNT") return Reply::Int(store->RawSubkeyCount());

  return Reply::Error("ERR unknown command '" + Lower(args[0]) + "'");
}

}  // namespace gamestore
