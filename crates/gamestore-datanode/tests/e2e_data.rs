//! End-to-end integration tests for the I-05 assembly: real TCP connections
//! speaking raw RESP against a served [`gamestore_datanode`] instance backed
//! by a RocksDB store.
//!
//! Covers the Phase-1 exit criteria at the wire level:
//! - data commands (String/Hash) round-trip through the command registry,
//! - `FLUSHDB` clears the database,
//! - data written before a server+store restart is still readable after
//!   ("restart does not lose persisted data"),
//! - graceful shutdown drains connections and `serve` returns.

use std::path::Path;
use std::time::Duration;

use gamestore_common::WalSettings;
use gamestore_engine::EngineConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A running server bound to an ephemeral port, with a handle to stop it.
struct TestServer {
    addr: std::net::SocketAddr,
    stop: tokio::sync::oneshot::Sender<()>,
    done: tokio::task::JoinHandle<std::io::Result<()>>,
}

async fn start_server(data_dir: &Path) -> TestServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Exercise the full WAL-backed Core (I-08): RocksDB under data_dir/engine,
    // WAL under data_dir/wal.
    let store =
        gamestore_datanode::open_core(data_dir, &EngineConfig::default(), &WalSettings::default())
            .unwrap();
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let done = tokio::spawn(async move {
        gamestore_datanode::serve(listener, store, async {
            let _ = stop_rx.await;
        })
        .await
    });
    TestServer { addr, stop, done }
}

impl TestServer {
    /// Signal shutdown and wait for `serve` to drain and return.
    async fn shutdown(self) {
        let _ = self.stop.send(());
        tokio::time::timeout(Duration::from_secs(10), self.done)
            .await
            .expect("serve did not stop after shutdown signal")
            .expect("serve task panicked")
            .expect("serve returned an error");
    }
}

/// Encode `parts` as a RESP array-of-bulk-strings request.
fn req(parts: &[&str]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", parts.len()).into_bytes();
    for p in parts {
        out.extend_from_slice(format!("${}\r\n{p}\r\n", p.len()).as_bytes());
    }
    out
}

/// Send one command and read one reply (bounded, single-frame replies only).
async fn call(stream: &mut TcpStream, parts: &[&str]) -> Vec<u8> {
    stream.write_all(&req(parts)).await.unwrap();
    stream.flush().await.unwrap();
    let mut buf = [0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("read timed out")
        .expect("read failed");
    buf[..n].to_vec()
}

#[tokio::test]
async fn string_and_hash_commands_over_tcp() {
    let dir = tempfile::TempDir::new().unwrap();
    let server = start_server(dir.path()).await;
    let mut c = TcpStream::connect(server.addr).await.unwrap();

    assert_eq!(call(&mut c, &["SET", "k1", "v1"]).await, b"+OK\r\n");
    assert_eq!(call(&mut c, &["GET", "k1"]).await, b"$2\r\nv1\r\n");
    assert_eq!(call(&mut c, &["GET", "missing"]).await, b"$-1\r\n");
    assert_eq!(call(&mut c, &["TYPE", "k1"]).await, b"+string\r\n");

    assert_eq!(
        call(&mut c, &["HSET", "h", "gold", "100", "level", "5"]).await,
        b":2\r\n"
    );
    assert_eq!(call(&mut c, &["HGET", "h", "gold"]).await, b"$3\r\n100\r\n");
    assert_eq!(call(&mut c, &["HLEN", "h"]).await, b":2\r\n");
    assert_eq!(
        call(&mut c, &["HMGET", "h", "gold", "nope"]).await,
        b"*2\r\n$3\r\n100\r\n$-1\r\n"
    );

    // WRONGTYPE propagates through the wire.
    let reply = call(&mut c, &["GET", "h"]).await;
    assert!(reply.starts_with(b"-WRONGTYPE"), "got {reply:?}");

    // Unknown commands use the canonical wording.
    let reply = call(&mut c, &["GEOADD", "g", "1", "2", "m"]).await;
    assert!(
        reply.starts_with(b"-ERR unknown command 'GEOADD'"),
        "got {reply:?}"
    );

    // FLUSHDB clears everything.
    assert_eq!(call(&mut c, &["FLUSHDB"]).await, b"+OK\r\n");
    assert_eq!(call(&mut c, &["DBSIZE"]).await, b":0\r\n");
    assert_eq!(call(&mut c, &["GET", "k1"]).await, b"$-1\r\n");

    server.shutdown().await;
}

#[tokio::test]
async fn hgetall_is_map_on_resp3_and_flat_array_on_resp2() {
    let dir = tempfile::TempDir::new().unwrap();
    let server = start_server(dir.path()).await;

    let mut c2 = TcpStream::connect(server.addr).await.unwrap();
    assert_eq!(call(&mut c2, &["HSET", "h", "f", "1"]).await, b":1\r\n");
    assert_eq!(
        call(&mut c2, &["HGETALL", "h"]).await,
        b"*2\r\n$1\r\nf\r\n$1\r\n1\r\n"
    );

    let mut c3 = TcpStream::connect(server.addr).await.unwrap();
    let hello = call(&mut c3, &["HELLO", "3"]).await;
    assert!(hello.starts_with(b"%"), "expected RESP3 map, got {hello:?}");
    assert_eq!(
        call(&mut c3, &["HGETALL", "h"]).await,
        b"%1\r\n$1\r\nf\r\n$1\r\n1\r\n"
    );

    server.shutdown().await;
}

/// I-06: the Set/ZSet/List families round-trip over the wire, including the
/// version-aware reply shapes.
#[tokio::test]
async fn composite_type_commands_over_tcp() {
    let dir = tempfile::TempDir::new().unwrap();
    let server = start_server(dir.path()).await;
    let mut c = TcpStream::connect(server.addr).await.unwrap();

    // Set.
    assert_eq!(call(&mut c, &["SADD", "s", "a", "b", "a"]).await, b":2\r\n");
    assert_eq!(call(&mut c, &["SISMEMBER", "s", "a"]).await, b":1\r\n");
    assert_eq!(call(&mut c, &["SCARD", "s"]).await, b":2\r\n");
    assert_eq!(call(&mut c, &["TYPE", "s"]).await, b"+set\r\n");

    // ZSet: score-ordered ranges, RESP2 bulk scores.
    assert_eq!(
        call(&mut c, &["ZADD", "z", "2", "b", "1", "a"]).await,
        b":2\r\n"
    );
    assert_eq!(
        call(&mut c, &["ZRANGE", "z", "0", "-1", "WITHSCORES"]).await,
        b"*4\r\n$1\r\na\r\n$1\r\n1\r\n$1\r\nb\r\n$1\r\n2\r\n"
    );
    assert_eq!(
        call(&mut c, &["ZRANGEBYSCORE", "z", "(1", "+inf"]).await,
        b"*1\r\n$1\r\nb\r\n"
    );
    assert_eq!(call(&mut c, &["ZSCORE", "z", "a"]).await, b"$1\r\n1\r\n");

    // List.
    assert_eq!(call(&mut c, &["RPUSH", "l", "x", "y"]).await, b":2\r\n");
    assert_eq!(call(&mut c, &["LPUSH", "l", "w"]).await, b":3\r\n");
    assert_eq!(
        call(&mut c, &["LRANGE", "l", "0", "-1"]).await,
        b"*3\r\n$1\r\nw\r\n$1\r\nx\r\n$1\r\ny\r\n"
    );
    assert_eq!(call(&mut c, &["LPOP", "l"]).await, b"$1\r\nw\r\n");
    assert_eq!(
        call(&mut c, &["RPOP", "l", "2"]).await,
        b"*2\r\n$1\r\ny\r\n$1\r\nx\r\n"
    );
    assert_eq!(call(&mut c, &["LPOP", "l"]).await, b"$-1\r\n");

    // WRONGTYPE propagates for the new families.
    let reply = call(&mut c, &["LPUSH", "s", "v"]).await;
    assert!(reply.starts_with(b"-WRONGTYPE"), "got {reply:?}");

    // RESP3 shapes: SMEMBERS is a native set, ZSCORE a native double.
    let mut c3 = TcpStream::connect(server.addr).await.unwrap();
    let hello = call(&mut c3, &["HELLO", "3"]).await;
    assert!(hello.starts_with(b"%"), "got {hello:?}");
    let smembers = call(&mut c3, &["SMEMBERS", "s"]).await;
    assert!(smembers.starts_with(b"~2\r\n"), "got {smembers:?}");
    assert_eq!(call(&mut c3, &["ZSCORE", "z", "a"]).await, b",1\r\n");

    server.shutdown().await;
}

/// Phase-1 exit criterion: data persisted before a full server + store restart
/// is still readable afterwards.
#[tokio::test]
async fn restart_preserves_persisted_data() {
    let dir = tempfile::TempDir::new().unwrap();

    {
        let server = start_server(dir.path()).await;
        let mut c = TcpStream::connect(server.addr).await.unwrap();
        assert_eq!(call(&mut c, &["SET", "k", "v"]).await, b"+OK\r\n");
        assert_eq!(
            call(&mut c, &["HSET", "player:1", "gold", "100", "hp", "42"]).await,
            b":2\r\n"
        );
        assert_eq!(call(&mut c, &["SADD", "guild", "alice"]).await, b":1\r\n");
        assert_eq!(call(&mut c, &["ZADD", "lb", "9", "alice"]).await, b":1\r\n");
        assert_eq!(call(&mut c, &["RPUSH", "log", "e1", "e2"]).await, b":2\r\n");
        drop(c);
        // Graceful shutdown closes the store (Arc dropped when serve returns).
        server.shutdown().await;
    }

    {
        let server = start_server(dir.path()).await;
        let mut c = TcpStream::connect(server.addr).await.unwrap();
        assert_eq!(call(&mut c, &["GET", "k"]).await, b"$1\r\nv\r\n");
        assert_eq!(
            call(&mut c, &["HGET", "player:1", "gold"]).await,
            b"$3\r\n100\r\n"
        );
        assert_eq!(call(&mut c, &["HLEN", "player:1"]).await, b":2\r\n");
        assert_eq!(
            call(&mut c, &["SISMEMBER", "guild", "alice"]).await,
            b":1\r\n"
        );
        assert_eq!(
            call(&mut c, &["ZSCORE", "lb", "alice"]).await,
            b"$1\r\n9\r\n"
        );
        assert_eq!(
            call(&mut c, &["LRANGE", "log", "0", "-1"]).await,
            b"*2\r\n$2\r\ne1\r\n$2\r\ne2\r\n"
        );
        assert_eq!(call(&mut c, &["DBSIZE"]).await, b":5\r\n");
        server.shutdown().await;
    }
}

/// I-08 crash recovery: writes acknowledged before a crash that loses the
/// engine's unflushed state are recovered from the WAL alone.
///
/// We model a hard crash (kill -9 before any checkpoint/flush) by wiping the
/// RocksDB directory (`data_dir/engine`) while keeping the fsync'd WAL
/// (`data_dir/wal`). On restart, `open_core` replays the WAL into a fresh empty
/// engine and every confirmed write is back — proving the WAL, not RocksDB's
/// own persistence, carries durability here (RocksDB's WAL is disabled on this
/// path).
#[tokio::test]
async fn wal_recovers_confirmed_writes_after_engine_loss() {
    let dir = tempfile::TempDir::new().unwrap();

    {
        let server = start_server(dir.path()).await;
        let mut c = TcpStream::connect(server.addr).await.unwrap();
        // Each reply is the client's confirmation the write is durable.
        assert_eq!(call(&mut c, &["SET", "coins", "500"]).await, b"+OK\r\n");
        assert_eq!(
            call(&mut c, &["HSET", "player:7", "hp", "88", "mp", "12"]).await,
            b":2\r\n"
        );
        assert_eq!(
            call(&mut c, &["ZADD", "board", "42", "neo"]).await,
            b":1\r\n"
        );
        drop(c);
        // Stop serving WITHOUT checkpointing (serve does not checkpoint; that is
        // main's clean-shutdown step, which a kill -9 never reaches).
        server.shutdown().await;
    }

    // Simulate the engine losing everything unflushed: delete the RocksDB dir.
    // The WAL directory is left intact.
    std::fs::remove_dir_all(dir.path().join("engine")).unwrap();
    assert!(
        dir.path().join("wal").exists(),
        "WAL must survive the crash"
    );

    {
        let server = start_server(dir.path()).await;
        let mut c = TcpStream::connect(server.addr).await.unwrap();
        assert_eq!(call(&mut c, &["GET", "coins"]).await, b"$3\r\n500\r\n");
        assert_eq!(
            call(&mut c, &["HGET", "player:7", "hp"]).await,
            b"$2\r\n88\r\n"
        );
        assert_eq!(call(&mut c, &["HLEN", "player:7"]).await, b":2\r\n");
        assert_eq!(
            call(&mut c, &["ZSCORE", "board", "neo"]).await,
            b"$2\r\n42\r\n"
        );
        assert_eq!(call(&mut c, &["DBSIZE"]).await, b":3\r\n");
        server.shutdown().await;
    }
}

/// Graceful shutdown: open connections are drained (not hung) and `serve`
/// returns even while a client is idle mid-connection.
#[tokio::test]
async fn graceful_shutdown_drains_idle_connections() {
    let dir = tempfile::TempDir::new().unwrap();
    let server = start_server(dir.path()).await;

    let mut c = TcpStream::connect(server.addr).await.unwrap();
    assert_eq!(call(&mut c, &["PING"]).await, b"+PONG\r\n");

    // Client stays connected and idle; shutdown must still complete.
    server.shutdown().await;

    // The server side closed the connection: the next read yields EOF.
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), c.read(&mut buf))
        .await
        .expect("read timed out")
        .expect("read failed");
    assert_eq!(n, 0, "expected EOF after server shutdown");
}
