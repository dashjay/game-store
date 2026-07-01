// GameStore Phase-1 spike (C++).
//
// A minimal RESP2 server backed by RocksDB that demonstrates the
// docs/design/03-storage-engine.md encoding (metadata + subkey + version) and
// version-based subkey GC via a RocksDB compaction filter. Functionally
// equivalent to the Rust spike under spike/rust/.
//
// Usage: gamestore_spike [--port N] [--db PATH]

#include <arpa/inet.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cstdio>
#include <cstring>
#include <string>
#include <thread>

#include "commands.h"
#include "resp.h"
#include "storage.h"

namespace {

bool WriteAll(int fd, const std::string& data) {
  size_t sent = 0;
  while (sent < data.size()) {
    ssize_t n = ::write(fd, data.data() + sent, data.size() - sent);
    if (n <= 0) {
      return false;
    }
    sent += static_cast<size_t>(n);
  }
  return true;
}

void HandleConn(int fd, gamestore::Store* store) {
  int one = 1;
  setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &one, sizeof(one));

  gamestore::RespReader reader(fd);
  std::vector<std::string> args;
  for (;;) {
    int rc = reader.ReadCommand(&args);
    if (rc == 0) break;       // EOF
    if (rc < 0) break;        // protocol error
    if (args.empty()) continue;

    bool close = false;
    gamestore::Reply reply = gamestore::Dispatch(store, args, &close);
    std::string out;
    reply.serialize(&out);
    if (!WriteAll(fd, out)) break;
    if (close) break;
  }
  ::close(fd);
}

}  // namespace

int main(int argc, char** argv) {
  int port = 6381;
  std::string db_path = "/tmp/gamestore-spike-cpp";

  for (int i = 1; i < argc; ++i) {
    if (std::strcmp(argv[i], "--port") == 0 && i + 1 < argc) {
      port = std::atoi(argv[++i]);
    } else if (std::strcmp(argv[i], "--db") == 0 && i + 1 < argc) {
      db_path = argv[++i];
    } else {
      fprintf(stderr, "unknown arg: %s\n", argv[i]);
    }
  }

  auto store = gamestore::Store::Open(db_path);
  if (!store) {
    return 1;
  }

  int listen_fd = ::socket(AF_INET, SOCK_STREAM, 0);
  if (listen_fd < 0) {
    perror("socket");
    return 1;
  }
  int opt = 1;
  setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

  sockaddr_in addr{};
  addr.sin_family = AF_INET;
  addr.sin_port = htons(static_cast<uint16_t>(port));
  inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);

  if (::bind(listen_fd, reinterpret_cast<sockaddr*>(&addr), sizeof(addr)) < 0) {
    perror("bind");
    return 1;
  }
  if (::listen(listen_fd, 128) < 0) {
    perror("listen");
    return 1;
  }

  printf("[cpp] GameStore spike listening on 127.0.0.1:%d (db=%s)\n", port,
         db_path.c_str());
  fflush(stdout);

  for (;;) {
    int conn_fd = ::accept(listen_fd, nullptr, nullptr);
    if (conn_fd < 0) {
      if (errno == EINTR) continue;
      perror("accept");
      break;
    }
    std::thread(HandleConn, conn_fd, store.get()).detach();
  }

  ::close(listen_fd);
  return 0;
}
