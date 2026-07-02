//! Smoke / integration test for I-01's exit criteria:
//! a standard RESP `PING` over TCP must get `+PONG`.
//!
//! We connect the same way a Redis client does — a RESP array of bulk strings —
//! and assert the raw wire reply, so this exercises the real accept loop and
//! protocol path (not just the `dispatch` unit tests).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn start_server() -> std::net::SocketAddr {
    // Bind to an ephemeral port so tests can run in parallel / on busy hosts.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    // Run the server until the test process exits; `pending` never resolves.
    tokio::spawn(async move {
        let _ = gamestore_datanode::serve(listener, std::future::pending::<()>()).await;
    });
    addr
}

async fn roundtrip(stream: &mut TcpStream, request: &[u8]) -> Vec<u8> {
    stream.write_all(request).await.unwrap();
    stream.flush().await.unwrap();
    let mut buf = [0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("read timed out")
        .expect("read failed");
    buf[..n].to_vec()
}

#[tokio::test]
async fn ping_returns_pong() {
    let addr = start_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    // RESP array: *1\r\n$4\r\nPING\r\n
    let reply = roundtrip(&mut stream, b"*1\r\n$4\r\nPING\r\n").await;
    assert_eq!(reply, b"+PONG\r\n", "expected +PONG, got {reply:?}");
}

#[tokio::test]
async fn inline_ping_returns_pong() {
    let addr = start_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    // Inline command (what `nc` / plain telnet would send).
    let reply = roundtrip(&mut stream, b"PING\r\n").await;
    assert_eq!(reply, b"+PONG\r\n", "expected +PONG, got {reply:?}");
}

#[tokio::test]
async fn ping_with_argument_is_echoed() {
    let addr = start_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let reply = roundtrip(&mut stream, b"*2\r\n$4\r\nPING\r\n$5\r\nhello\r\n").await;
    assert_eq!(
        reply, b"$5\r\nhello\r\n",
        "expected bulk 'hello', got {reply:?}"
    );
}

#[tokio::test]
async fn echo_returns_bulk() {
    let addr = start_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let reply = roundtrip(&mut stream, b"*2\r\n$4\r\nECHO\r\n$2\r\nhi\r\n").await;
    assert_eq!(reply, b"$2\r\nhi\r\n", "expected bulk 'hi', got {reply:?}");
}

#[tokio::test]
async fn hello_default_returns_resp2_array_map() {
    let addr = start_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let reply = roundtrip(&mut stream, b"*1\r\n$5\r\nHELLO\r\n").await;
    // RESP2 flattened map: an array whose first entry is `server`.
    assert!(reply.starts_with(b"*"), "expected array, got {reply:?}");
    assert!(
        contains(&reply, b"server") && contains(&reply, b"gamestore"),
        "hello reply missing server info: {reply:?}"
    );
}

#[tokio::test]
async fn hello_3_switches_to_resp3_map() {
    let addr = start_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    let reply = roundtrip(&mut stream, b"*2\r\n$5\r\nHELLO\r\n$1\r\n3\r\n").await;
    // RESP3 map replies start with `%`.
    assert!(reply.starts_with(b"%"), "expected RESP3 map, got {reply:?}");
    assert!(
        contains(&reply, b"proto"),
        "hello reply missing proto: {reply:?}"
    );
}

#[tokio::test]
async fn inline_command_with_arg_works() {
    let addr = start_server().await;
    let mut stream = TcpStream::connect(addr).await.unwrap();

    // Inline (non-array) form — what telnet/nc would type.
    let reply = roundtrip(&mut stream, b"ECHO world\r\n").await;
    assert_eq!(reply, b"$5\r\nworld\r\n", "got {reply:?}");
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
