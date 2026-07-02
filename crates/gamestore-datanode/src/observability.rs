//! DataNode observability (I-07): command metrics, the Redis-style slow log
//! and the Prometheus `/metrics` HTTP endpoint.
//!
//! Metric taxonomy follows [`docs/design/08-observability-ops.md`] §1, scoped
//! to what exists in the Phase-1 single-node DataNode (no Proxy, no quorum
//! yet — those metrics arrive with their features):
//!
//! - `datanode_commands_total{cmd}` — per-command throughput (QPS via
//!   `rate()`), the single-node counterpart of `proxy_qps{cmd}`.
//! - `datanode_command_latency_seconds{cmd,quantile}` — per-command latency
//!   summary (p50/p90/p95/p99/p999), counterpart of `proxy_latency_seconds`.
//! - `datanode_slow_commands_total{cmd}` — commands over the slow-log
//!   threshold.
//! - `datanode_conn_active` — open RESP connections (`proxy_conn_active`
//!   analogue).
//! - `rocksdb_*` gauges — engine statistics ([`GeneralEngine::stats`]):
//!   block-cache usage, write-stall state, memtable/compaction pressure,
//!   SST footprint (08 §1.2's `rocksdb_*` / `disk_used_bytes` signals).
//!
//! The HTTP endpoint is a **hand-rolled minimal HTTP/1.1 responder** rather
//! than `metrics-exporter-prometheus`'s optional `http-listener` feature: that
//! feature pulls the full hyper stack for what is one fixed `GET /metrics`
//! route, and our pinned Rust 1.83 toolchain makes every transitive
//! dependency a liability (see the `edition2024` pins in the root manifest).
//! Serving a fixed text body needs ~60 lines of tokio and no new
//! dependencies.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use gamestore_engine::{GeneralEngine, Store};
use metrics_exporter_prometheus::PrometheusHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Register help text for the DataNode metric set (idempotent).
pub fn describe_metrics() {
    metrics::describe_counter!(
        "datanode_commands_total",
        "Commands executed, labelled by (known) command name"
    );
    metrics::describe_histogram!(
        "datanode_command_latency_seconds",
        metrics::Unit::Seconds,
        "Command execution latency (dispatch to reply frame ready)"
    );
    metrics::describe_counter!(
        "datanode_slow_commands_total",
        "Commands that exceeded the slow-log threshold"
    );
    metrics::describe_gauge!("datanode_conn_active", "Open RESP connections");
}

/// Record one executed command: throughput counter, latency histogram and —
/// past `slow_threshold` — a slow-log entry (Redis's `slowlog-log-slower-than`
/// analogue, emitted as a structured `WARN` on the `gamestore::slowlog`
/// target) plus the slow-command counter.
///
/// `cmd` must be a **bounded** label value: pass the canonical uppercase name
/// for registered commands and `"UNKNOWN"` otherwise, so arbitrary client
/// input can never explode the label cardinality.
pub fn record_command(cmd: String, args: &[Bytes], elapsed: Duration, slow_threshold: Duration) {
    metrics::counter!("datanode_commands_total", "cmd" => cmd.clone()).increment(1);
    metrics::histogram!("datanode_command_latency_seconds", "cmd" => cmd.clone())
        .record(elapsed.as_secs_f64());
    if elapsed >= slow_threshold {
        metrics::counter!("datanode_slow_commands_total", "cmd" => cmd.clone()).increment(1);
        // Redis's slow log records the command and its (truncated) arguments;
        // we log the key (args[1]) — values may be large and sensitive.
        let key = args
            .get(1)
            .map(|k| String::from_utf8_lossy(k).into_owned())
            .unwrap_or_default();
        tracing::warn!(
            target: "gamestore::slowlog",
            cmd = %cmd,
            key = %key,
            argc = args.len(),
            elapsed_ms = elapsed.as_millis() as u64,
            "slow command"
        );
    }
}

/// Push the engine's point-in-time statistics into the metrics registry.
fn refresh_engine_gauges<E: GeneralEngine>(store: &Store<E>) {
    for (name, value) in store.engine().stats() {
        metrics::gauge!(name).set(value as f64);
    }
}

/// Serve the Prometheus exposition endpoint until `shutdown` resolves.
///
/// Only `GET /metrics` exists; everything else is a 404. Engine gauges are
/// refreshed and the recorder's upkeep is run on each scrape, so gauge
/// freshness matches the scrape interval without a background sampler task.
pub async fn serve_metrics<E, S>(
    listener: TcpListener,
    handle: PrometheusHandle,
    store: Arc<Store<E>>,
    shutdown: S,
) -> std::io::Result<()>
where
    E: GeneralEngine + 'static,
    S: std::future::Future<Output = ()>,
{
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else { continue };
                let handle = handle.clone();
                let store = store.clone();
                tokio::spawn(async move {
                    if let Err(e) = answer_http(stream, &handle, &store).await {
                        tracing::debug!(%peer, error = %e, "metrics connection error");
                    }
                });
            }
        }
    }
}

/// Answer a single HTTP request on `stream` and close it.
async fn answer_http<E: GeneralEngine>(
    mut stream: tokio::net::TcpStream,
    handle: &PrometheusHandle,
    store: &Store<E>,
) -> std::io::Result<()> {
    // Read the request head (bounded; we only need the request line).
    let mut buf = vec![0u8; 4096];
    let mut read = 0;
    while read < buf.len() && !buf[..read].windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut buf[read..]).await?;
        if n == 0 {
            break;
        }
        read += n;
    }
    let head = String::from_utf8_lossy(&buf[..read]);
    let mut parts = head.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));

    let response = if method == "GET" && (path == "/metrics" || path.starts_with("/metrics?")) {
        refresh_engine_gauges(store);
        handle.run_upkeep();
        let body = handle.render();
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/plain; version=0.0.4; charset=utf-8\r\n\
             content-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        )
    } else {
        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_string()
    };
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}
