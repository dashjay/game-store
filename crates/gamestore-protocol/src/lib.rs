//! `gamestore-protocol` — the RESP2/RESP3 wire protocol for GameStore.
//!
//! This crate is the接入层 codec: it turns bytes on the wire into structured
//! [`Frame`] values (and client requests into argument vectors) and back. It is
//! split into a **sans-IO core** and a thin **tokio adapter** so the parsing
//! logic can be exhaustively unit- and property-tested without any sockets:
//!
//! - [`frame`] — the [`Frame`] value model (RESP2 ∪ RESP3) and [`RespVersion`].
//! - [`decode`] — incremental, allocation-bounded decoder ([`decode`],
//!   [`decode_command`], [`Limits`]). Handles fragmented reads by returning
//!   `Ok(None)` until a full frame is present.
//! - [`encode`] — version-aware serializer ([`encode`]).
//! - [`codec`] — [`tokio_util::codec`] adapters ([`RespCodec`], [`CommandCodec`]).
//! - [`error`] — [`ProtocolError`].
//!
//! # Protocol coverage
//!
//! RESP2: simple strings, errors, integers, bulk strings (incl. null bulk),
//! arrays (incl. null array), and inline commands. RESP3 adds nulls (`_`),
//! booleans, doubles, big numbers, bulk errors, verbatim strings, maps, sets and
//! push messages — enough to serve the `HELLO 3` handshake and typed replies.
//!
//! # Example
//!
//! ```
//! use bytes::BytesMut;
//! use gamestore_protocol::{decode_command, encode, Frame, Limits, RespVersion};
//!
//! // A client sends `PING` as a RESP array of one bulk string.
//! let mut buf = BytesMut::from(&b"*1\r\n$4\r\nPING\r\n"[..]);
//! let args = decode_command(&mut buf, &Limits::default()).unwrap().unwrap();
//! assert_eq!(args, vec![bytes::Bytes::from_static(b"PING")]);
//!
//! // The server replies with a simple string.
//! let mut out = BytesMut::new();
//! encode(&Frame::simple("PONG"), RespVersion::V2, &mut out);
//! assert_eq!(&out[..], b"+PONG\r\n");
//! ```
#![forbid(unsafe_code)]

pub mod codec;
pub mod decode;
pub mod encode;
pub mod error;
pub mod frame;

pub use codec::{CodecError, CommandCodec, RespCodec};
pub use decode::{decode, decode_command, Limits};
pub use encode::{encode, encode_to_vec};
pub use error::ProtocolError;
pub use frame::{Frame, RespVersion};

/// Crate name, kept for the I-01 wiring assertions until they are retired.
pub const CRATE_NAME: &str = "gamestore-protocol";
