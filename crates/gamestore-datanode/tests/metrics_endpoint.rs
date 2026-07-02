//! Integration test for the I-07 observability wiring: a real RESP server plus
//! the `/metrics` HTTP endpoint sharing one process-global Prometheus
//! recorder.
//!
//! Everything lives in a single #[tokio::test] because the `metrics` recorder
//! is a process-global singleton (one install per test binary).

use std::sync::Arc;
use std::time::Duration;

use gamestore_engine::{EngineConfig, Store};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Encode `parts` as a RESP array-of-bulk-strings request.
fn req(parts: &[&str]) -> Vec<u8> {
    let mut out = format!("*{}\r\n", parts.len()).into_bytes();
    for p in parts {
        out.extend_from_slice(format!("${}\r\n{p}\r\n", p.len()).as_bytes());
    }
    out
}

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

/// One HTTP GET against the metrics listener, returning the raw response.
async fn http_get(addr: std::net::SocketAddr, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\nhost: test\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut body = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut body))
        .await
        .expect("read timed out")
        .expect("read failed");
    String::from_utf8_lossy(&body).into_owned()
}

#[tokio::test]
async fn metrics_endpoint_exposes_command_and_engine_metrics() {
    let recorder = gamestore_common::metrics::init().expect("install recorder");
    gamestore_datanode::observability::describe_metrics();

    let dir = tempfile::TempDir::new().unwrap();
    let store = Arc::new(Store::open(dir.path(), &EngineConfig::default()).unwrap());

    // RESP server with an aggressive slow-log threshold so COMPACT trips it.
    let resp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let resp_addr = resp_listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(gamestore_datanode::serve_with(
        resp_listener,
        store.clone(),
        gamestore_datanode::ServeOptions {
            slow_log_threshold: Duration::from_nanos(1),
        },
        async {
            let _ = stop_rx.await;
        },
    ));

    // Metrics endpoint.
    let metrics_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let metrics_addr = metrics_listener.local_addr().unwrap();
    tokio::spawn(gamestore_datanode::serve_metrics(
        metrics_listener,
        recorder,
        store.clone(),
        std::future::pending(),
    ));

    // Generate some traffic: known commands, an unknown command.
    let mut c = TcpStream::connect(resp_addr).await.unwrap();
    assert_eq!(call(&mut c, &["SET", "k", "v"]).await, b"+OK\r\n");
    assert_eq!(call(&mut c, &["GET", "k"]).await, b"$1\r\nv\r\n");
    assert_eq!(call(&mut c, &["ZADD", "z", "1", "m"]).await, b":1\r\n");
    let _ = call(&mut c, &["NOSUCHCMD"]).await;

    let response = http_get(metrics_addr, "/metrics").await;
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "status line: {}",
        response.lines().next().unwrap_or("")
    );
    assert!(response.contains("text/plain"), "content type header");

    // Command throughput counters with bounded labels.
    assert!(
        response.contains(r#"datanode_commands_total{cmd="SET"} 1"#),
        "SET counter missing:\n{response}"
    );
    assert!(
        response.contains(r#"datanode_commands_total{cmd="ZADD"} 1"#),
        "ZADD counter missing"
    );
    assert!(
        response.contains(r#"cmd="UNKNOWN""#),
        "unknown commands must fold into the UNKNOWN label"
    );
    assert!(
        !response.contains("NOSUCHCMD"),
        "arbitrary input must not mint metric labels"
    );

    // Latency summary with quantile labels (08 §1.1 shape).
    assert!(
        response.contains("datanode_command_latency_seconds"),
        "latency summary missing"
    );
    assert!(response.contains(r#"quantile="0.99""#), "p99 missing");

    // Slow log counter (threshold is 1ns, so everything is slow).
    assert!(
        response.contains("datanode_slow_commands_total"),
        "slow command counter missing"
    );

    // Connection gauge and engine statistics gauges.
    assert!(response.contains("datanode_conn_active"), "conn gauge");
    assert!(
        response.contains("rocksdb_estimate_num_keys"),
        "engine stats gauges missing:\n{response}"
    );
    assert!(response.contains("rocksdb_block_cache_usage_bytes"));

    // Anything but GET /metrics is a 404.
    let response = http_get(metrics_addr, "/nope").await;
    assert!(response.starts_with("HTTP/1.1 404"), "got {response}");

    let _ = stop_tx.send(());
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("server did not stop")
        .expect("server panicked")
        .expect("server errored");
}
