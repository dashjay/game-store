//! Protocol-layer error type.
//!
//! `gamestore-protocol` is a leaf crate with its own concrete, matchable error
//! ([`ProtocolError`]). A blanket `From<ProtocolError>` into the workspace-wide
//! [`gamestore_common::Error`] is provided so callers can bubble protocol
//! failures up without hand-writing conversions.

use thiserror::Error;

/// Anything that can go wrong while decoding/encoding RESP.
///
/// `#[non_exhaustive]` so future MRs can add variants without breaking
/// downstream `match` arms.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProtocolError {
    /// The bytes on the wire are not valid RESP (bad type byte, malformed
    /// header, missing CRLF, non-numeric length, …). This is unrecoverable for
    /// the connection: the peer is out of sync and should be disconnected.
    #[error("protocol error: {0}")]
    Malformed(String),

    /// A declared length (bulk string, verbatim string, multibulk count)
    /// exceeded the configured safety limit. Rejected to avoid unbounded
    /// allocation from a hostile or buggy peer.
    #[error("value exceeds configured limit: {0}")]
    LimitExceeded(String),

    /// An inline command could not be tokenized (e.g. an unbalanced quote).
    #[error("invalid inline command: {0}")]
    InlineSyntax(String),
}

impl ProtocolError {
    /// Build a [`ProtocolError::Malformed`] from anything printable.
    pub(crate) fn malformed(msg: impl std::fmt::Display) -> Self {
        ProtocolError::Malformed(msg.to_string())
    }
}

impl From<ProtocolError> for gamestore_common::Error {
    fn from(e: ProtocolError) -> Self {
        gamestore_common::Error::Protocol(e.to_string())
    }
}
