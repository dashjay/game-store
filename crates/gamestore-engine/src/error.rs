//! Engine-layer error type.
//!
//! `gamestore-engine` defines its own concrete, matchable error
//! ([`EngineError`]) per the plan (§1: library crates use `thiserror`). A
//! blanket `From<EngineError>` into the workspace-wide [`gamestore_common::Error`]
//! lets callers bubble engine failures up without hand-written conversions.

use thiserror::Error;

/// Result alias for engine operations.
pub type Result<T> = std::result::Result<T, EngineError>;

/// Anything that can go wrong in the general engine layer.
///
/// `#[non_exhaustive]` so future MRs can add variants (e.g. corruption, quota)
/// without breaking downstream `match` arms.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The underlying storage backend (RocksDB) returned an error.
    #[error("engine backend error: {0}")]
    Backend(String),

    /// A stored record could not be decoded (unexpected/corrupt layout).
    #[error("engine corruption: {0}")]
    Corruption(String),

    /// The requested operation targets a key of an incompatible Redis type
    /// (e.g. `HGET` against a String). Mirrors Redis `WRONGTYPE`.
    #[error("WRONGTYPE Operation against a key holding the wrong kind of value")]
    WrongType,
}

impl EngineError {
    /// Build an [`EngineError::Backend`] from anything printable.
    pub fn backend(msg: impl std::fmt::Display) -> Self {
        EngineError::Backend(msg.to_string())
    }

    /// Build an [`EngineError::Corruption`] from anything printable.
    pub fn corruption(msg: impl std::fmt::Display) -> Self {
        EngineError::Corruption(msg.to_string())
    }
}

impl From<rocksdb::Error> for EngineError {
    fn from(e: rocksdb::Error) -> Self {
        EngineError::Backend(e.to_string())
    }
}

impl From<EngineError> for gamestore_common::Error {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::WrongType => gamestore_common::Error::Engine(e.to_string()),
            other => gamestore_common::Error::Engine(other.to_string()),
        }
    }
}
