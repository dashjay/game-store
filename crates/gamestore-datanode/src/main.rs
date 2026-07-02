//! `gamestore-datanode` binary entry point.
//!
//! Loads configuration, initializes observability, opens the store and binds
//! the RESP listener, then serves until Ctrl-C. Thin on purpose — the server
//! logic lives in the library so it can be integration-tested.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use gamestore_common::Config;
use gamestore_engine::{EngineConfig, Store};
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

    if cfg.metrics.enabled {
        match gamestore_common::metrics::init() {
            Ok(_handle) => tracing::info!("prometheus metrics recorder installed"),
            Err(e) => tracing::warn!(error = %e, "failed to install metrics recorder"),
        }
    }

    // Open the store (one per DataNode; see the `Core` note in `server`). All
    // connections share it via Arc.
    let data_dir = cfg.storage.data_dir.clone();
    std::fs::create_dir_all(&data_dir)
        .with_context(|| format!("creating data directory {}", data_dir.display()))?;
    let store = Arc::new(
        Store::open(&data_dir, &EngineConfig::default())
            .with_context(|| format!("opening store at {}", data_dir.display()))?,
    );
    tracing::info!(data_dir = %data_dir.display(), "store opened");

    let addr = cfg.server.addr();
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding RESP listener on {addr}"))?;
    tracing::info!(%addr, "gamestore-datanode listening (I-05 single-node MVP)");

    gamestore_datanode::serve(listener, store, shutdown_signal())
        .await
        .context("serving connections")?;

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
