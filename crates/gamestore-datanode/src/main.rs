//! `gamestore-datanode` binary entry point.
//!
//! Loads configuration, initializes observability, binds the RESP listener and
//! serves until Ctrl-C. Thin on purpose — the server logic lives in the library
//! so it can be integration-tested.

use std::path::PathBuf;

use anyhow::Context;
use gamestore_common::Config;
use tokio::net::TcpListener;

/// Parsed command-line arguments.
struct Args {
    config: Option<PathBuf>,
    bind: Option<String>,
    port: Option<u16>,
}

fn parse_args() -> anyhow::Result<Args> {
    let mut args = Args {
        config: None,
        bind: None,
        port: None,
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
            "-h" | "--help" => {
                println!(
                    "gamestore-datanode [--config PATH] [--bind ADDR] [--port PORT]\n\n\
                     Minimal RESP server (I-01): answers PING with PONG."
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

    let addr = cfg.server.addr();
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding RESP listener on {addr}"))?;
    tracing::info!(%addr, "gamestore-datanode listening (I-01: PING/ECHO/QUIT)");

    gamestore_datanode::serve(listener, shutdown_signal())
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
