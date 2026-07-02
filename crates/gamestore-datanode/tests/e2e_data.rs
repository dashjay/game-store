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
use std::sync::Arc;
use std::time::Duration;

use gamestore_engine::{EngineConfig, Store};
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
    let store = Arc::new(Store::open(data_dir, &EngineConfig::default()).unwrap());
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
    let reply = call(&mut c, &["LPUSH", "l", "x"]).await;
    assert!(
        reply.starts_with(b"-ERR unknown command 'LPUSH'"),
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
        assert_eq!(call(&mut c, &["DBSIZE"]).await, b":2\r\n");
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
