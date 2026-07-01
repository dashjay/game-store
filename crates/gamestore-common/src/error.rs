//! Unified error type shared across GameStore library crates.
//!
//! Per the plan (§1) library crates expose a concrete, matchable error type via
//! `thiserror`; binary crates aggregate context with `anyhow`. Downstream crates
//! add their own domain variants over time; this is the common root.

use thiserror::Error;

/// Convenience result alias used throughout the workspace.
pub type Result<T> = std::result::Result<T, Error>;

/// The unified GameStore error type.
///
/// Marked `#[non_exhaustive]` so new variants can be added in later MRs without
/// breaking downstream `match` arms.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// Invalid or unreadable configuration.
    #[error("configuration error: {0}")]
    Config(String),

    /// Underlying I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// RESP / wire protocol violation (fleshed out in I-02).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// Storage engine failure (fleshed out in I-03).
    #[error("engine error: {0}")]
    Engine(String),

    /// Observability subsystem (tracing / metrics) setup failure.
    #[error("observability error: {0}")]
    Observability(String),

    /// Anything not yet given a dedicated variant.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Build a [`Error::Config`] from anything printable.
    pub fn config(msg: impl std::fmt::Display) -> Self {
        Error::Config(msg.to_string())
    }

    /// Build a [`Error::Other`] from anything printable.
    pub fn other(msg: impl std::fmt::Display) -> Self {
        Error::Other(msg.to_string())
    }
}
