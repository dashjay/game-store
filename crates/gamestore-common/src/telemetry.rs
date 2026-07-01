//! Structured logging façade built on `tracing` + `tracing-subscriber`.
//!
//! Aligns with `docs/design/08-observability-ops.md`. I-01 provides just the
//! initialization entry point; richer spans/fields are added as the DataNode
//! grows.

use tracing_subscriber::{fmt, EnvFilter};

use crate::error::{Error, Result};

/// Initialize the global `tracing` subscriber.
///
/// The filter comes from the `RUST_LOG` environment variable when set,
/// otherwise from `default_directive` (e.g. the configured log level). Safe to
/// call once at process startup; calling it again returns an error rather than
/// panicking, so tests can tolerate a previously-installed subscriber.
pub fn init(default_directive: &str) -> Result<()> {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(default_directive))
        .map_err(|e| Error::Observability(format!("invalid log filter: {e}")))?;

    fmt()
        .with_env_filter(filter)
        .with_target(true)
        .try_init()
        .map_err(|e| Error::Observability(format!("installing tracing subscriber: {e}")))
}
