//! WAL-layer error type.
//!
//! `gamestore-wal` defines its own concrete, matchable error ([`WalError`]) per
//! the plan (§1: library crates use `thiserror`). A blanket `From<WalError>`
//! into the workspace-wide [`gamestore_common::Error`] lets callers bubble WAL
//! failures up without hand-written conversions.

use thiserror::Error;

/// Result alias for WAL operations.
pub type Result<T> = std::result::Result<T, WalError>;

/// Anything that can go wrong in the write-ahead log layer.
///
/// `#[non_exhaustive]` so future MRs can add variants without breaking
/// downstream `match` arms.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WalError {
    /// Underlying filesystem / I/O failure.
    #[error("wal i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// A WAL segment or record could not be decoded (unexpected/corrupt layout
    /// that is *not* a recoverable torn tail — see [`crate::file`] for how torn
    /// tails are handled by truncation rather than an error).
    #[error("wal corruption: {0}")]
    Corruption(String),

    /// The engine rejected a replayed record while recovering.
    #[error("wal replay into engine failed: {0}")]
    Replay(String),
}

impl WalError {
    /// Build a [`WalError::Corruption`] from anything printable.
    pub fn corruption(msg: impl std::fmt::Display) -> Self {
        WalError::Corruption(msg.to_string())
    }
}

impl From<WalError> for gamestore_common::Error {
    fn from(e: WalError) -> Self {
        gamestore_common::Error::other(format!("wal: {e}"))
    }
}

impl From<WalError> for gamestore_engine::EngineError {
    fn from(e: WalError) -> Self {
        // A WAL failure surfaces to the Store as a backend error: the write
        // could not be made durable, so it must not be reported as applied.
        gamestore_engine::EngineError::backend(format!("wal: {e}"))
    }
}
