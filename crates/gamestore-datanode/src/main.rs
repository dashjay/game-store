//! `gamestore-datanode` binary entry point.
//!
//! Loads configuration, initializes observability, opens the store and binds
//! the RESP listener, then serves until Ctrl-C. Thin on purpose — the server
//! logic lives in the library so it can be integration-tested.

use std::path::PathBuf;

use anyhow::Context;
use gamestore_common::Config;
use gamestore_engine::EngineConfig;
use tokio::net::TcpListener;

/// Parsed command-line arguments.
struct Args {
    config: Option<PathBuf>,
    bind: Option<String>,
    port: Option<u16>,
    data_dir: Option<PathBuf>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        config: None,
        bind: None,
        port: None,
        data_dir: None,
    };
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--config" if i + 1 < argv.len() => {
                args.config = Some(PathBuf::from(&argv[i + 1]));
                i += 2;
            }
            "--bind" if i + 1 < argv.len() => {
                args.bind = Some(argv[i + 1].clone());
                i += 2;
            }
            "--port" if i + 1 < argv.len() => {
                args.port = Some(argv[i + 1].parse().context("invalid --port")?);
                i += 2;
            }
            "--data-dir" if i + 1 < argv.len() => {
                args.data_dir = Some(PathBuf::from(&argv[i + 1]));
                i += 2;
            }
            "-h" | "--help" => {
                println!(
                    "gamestore-datanode [--config PATH] [--bind ADDR] [--port PORT] [--data-dir DIR]\n\n\
                     Single-node GameStore DataNode (I-05): RESP2/RESP3 server with the\n\
                     String/Hash/TTL command set persisted to RocksDB under --data-dir."
                );
                std::process::exit(0);
            }
            other => {
                anyhow::bail!("unknown argument: {other}");
            }
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = parse_args()?;

    let mut cfg = Config::load(args.config.as_deref()).context("loading configuration")?;
    // CLI flags win over file/env so ad-hoc runs are easy.
    if let Some(bind) = args.bind {
        cfg.server.bind = bind;
    }
    if let Some(port) = args.port {
        cfg.server.port = port;
    }
    if let Some(dir) = args.data_dir {
        cfg.storage.data_dir = dir;
    }

    // Initialize logging. A pre-installed subscriber (e.g. in tests) is not fatal.
    if let Err(e) = gamestore_common::telemetry::init(&cfg.logging.level) {
        eprintln!("warning: {e}");
    }

    let metrics_handle = if cfg.metrics.enabled {
        match gamestore_common::metrics::init() {
            Ok(handle) => {
                gamestore_datanode::observability::describe_metrics();
                tracing::info!("prometheus metrics recorder installed");
                Some(handle)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to install metrics recorder");
                None
            }
        }
    } else {
        None
    };

    // Open the DataNode's Core: the RocksDB store fronted by a per-Core WAL
    // (I-08). One Core per DataNode today; all connections share it via Arc
    // (see the `Core` note in `server`). On open, the WAL is replayed into the
    // engine so no confirmed write is lost across a crash.
    let data_dir = cfg.storage.data_dir.clone();
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    let store = gamestore_datanode::open_core(&data_dir, &EngineConfig::default(), &cfg.wal)
        .with_context(|| format!("opening core at {}", data_dir.display()))?;
    tracing::info!(
        data_dir = %data_dir.display(),
        wal_enabled = cfg.wal.enabled,
        "core opened"
    );

    // The /metrics endpoint (I-07) runs alongside the RESP listener and stops
    // with the process (it holds no state that needs draining).
    if let Some(handle) = metrics_handle {
        let metrics_addr = cfg.metrics.addr();
        let metrics_listener = TcpListener::bind(&metrics_addr)
            .await
            .with_context(|| format!("binding /metrics listener on {metrics_addr}"))?;
        tracing::info!(addr = %metrics_addr, "/metrics endpoint listening");
        let metrics_store = store.clone();
        tokio::spawn(async move {
            if let Err(e) = gamestore_datanode::serve_metrics(
                metrics_listener,
                handle,
                metrics_store,
                std::future::pending(),
            )
            .await
            {
                tracing::warn!(error = %e, "/metrics endpoint stopped");
            }
        });
    }

    let addr = cfg.server.addr();
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding RESP listener on {addr}"))?;
    tracing::info!(%addr, "gamestore-datanode listening");

    let options = gamestore_datanode::ServeOptions {
        slow_log_threshold: std::time::Duration::from_millis(cfg.logging.slow_log_threshold_ms),
    };
    // Keep a handle to checkpoint the WAL on a clean shutdown.
    let core = store.clone();
    gamestore_datanode::serve_with(listener, store, options, shutdown_signal())
        .await
        .context("serving connections")?;

    // Graceful shutdown: flush the engine and GC the WAL so the next start
    // replays little to nothing. A failure here is non-fatal (recovery still
    // works from the un-truncated log).
    if let Err(e) = core.engine().checkpoint() {
        tracing::warn!(error = %e, "wal checkpoint on shutdown failed");
    }

    tracing::info!("gamestore-datanode stopped");
    Ok(())
}

/// Resolve when a shutdown signal (Ctrl-C) is received.
async fn shutdown_signal() {
    if let Err(e) = tokio::signal::ctrl_c().await {
        tracing::warn!(error = %e, "failed to listen for ctrl-c; shutdown disabled");
        // Never resolve so the server keeps running rather than exiting on error.
        std::future::pending::<()>().await;
    }
}
