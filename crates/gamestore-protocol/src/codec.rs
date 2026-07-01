//! tokio adapter: [`tokio_util::codec`] `Decoder` / `Encoder` implementations.
//!
//! These wrap the sans-IO [`crate::decode`] / [`crate::encode`] core so a
//! connection can be driven with [`tokio_util::codec::Framed`]. The sans-IO core
//! stays independent of tokio (and thus trivially unit-testable); this module is
//! the only place that touches the async framing types.

use bytes::{Bytes, BytesMut};
use thiserror::Error;
use tokio_util::codec::{Decoder, Encoder};

use crate::decode::{decode, decode_command, Limits};
use crate::encode::encode;
use crate::error::ProtocolError;
use crate::frame::{Frame, RespVersion};

/// Error surfaced by the framed codecs: either a protocol violation or an I/O
/// error from the underlying transport.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CodecError {
    /// A RESP protocol violation. The connection should be closed.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// Transport I/O failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
}

impl From<CodecError> for gamestore_common::Error {
    fn from(e: CodecError) -> Self {
        match e {
            CodecError::Protocol(p) => p.into(),
            CodecError::Io(io) => gamestore_common::Error::Io(io),
        }
    }
}

/// Codec that decodes arbitrary RESP [`Frame`]s and encodes [`Frame`] replies.
///
/// The encode side is version-aware: set the negotiated [`RespVersion`] (e.g.
/// after a successful `HELLO 3`) with [`RespCodec::set_version`] so nulls and
/// RESP3-only types are written correctly.
#[derive(Debug, Clone)]
pub struct RespCodec {
    version: RespVersion,
    limits: Limits,
}

impl RespCodec {
    /// A RESP2 codec with default [`Limits`].
    pub fn new() -> Self {
        RespCodec {
            version: RespVersion::V2,
            limits: Limits::default(),
        }
    }

    /// Override the safety limits.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The protocol version used when encoding replies.
    pub fn version(&self) -> RespVersion {
        self.version
    }

    /// Switch the encode-side protocol version (typically after `HELLO`).
    pub fn set_version(&mut self, version: RespVersion) {
        self.version = version;
    }
}

impl Default for RespCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for RespCodec {
    type Item = Frame;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Frame>, CodecError> {
        Ok(decode(src, &self.limits)?)
    }
}

impl Encoder<Frame> for RespCodec {
    type Error = CodecError;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), CodecError> {
        encode(&item, self.version, dst);
        Ok(())
    }
}

impl Encoder<&Frame> for RespCodec {
    type Error = CodecError;

    fn encode(&mut self, item: &Frame, dst: &mut BytesMut) -> Result<(), CodecError> {
        encode(item, self.version, dst);
        Ok(())
    }
}

/// Server-side codec: decodes client *requests* (RESP multibulk or inline) into
/// argument vectors, and encodes [`Frame`] replies back.
///
/// This is what a DataNode connection loop uses: `Framed<TcpStream, CommandCodec>`
/// yields `Vec<Bytes>` commands and accepts `Frame` replies.
#[derive(Debug, Clone)]
pub struct CommandCodec {
    version: RespVersion,
    limits: Limits,
}

impl CommandCodec {
    /// A RESP2 command codec with default [`Limits`].
    pub fn new() -> Self {
        CommandCodec {
            version: RespVersion::V2,
            limits: Limits::default(),
        }
    }

    /// Override the safety limits.
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// The protocol version used when encoding replies.
    pub fn version(&self) -> RespVersion {
        self.version
    }

    /// Switch the encode-side protocol version (typically after `HELLO 3`).
    pub fn set_version(&mut self, version: RespVersion) {
        self.version = version;
    }
}

impl Default for CommandCodec {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for CommandCodec {
    type Item = Vec<Bytes>;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Vec<Bytes>>, CodecError> {
        Ok(decode_command(src, &self.limits)?)
    }
}

impl Encoder<Frame> for CommandCodec {
    type Error = CodecError;

    fn encode(&mut self, item: Frame, dst: &mut BytesMut) -> Result<(), CodecError> {
        encode(&item, self.version, dst);
        Ok(())
    }
}

impl Encoder<&Frame> for CommandCodec {
    type Error = CodecError;

    fn encode(&mut self, item: &Frame, dst: &mut BytesMut) -> Result<(), CodecError> {
        encode(item, self.version, dst);
        Ok(())
    }
}
