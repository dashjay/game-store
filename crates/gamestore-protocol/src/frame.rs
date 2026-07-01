//! The RESP value model.
//!
//! [`Frame`] is the single logical value type used for **both** RESP2 and RESP3.
//! Where the two protocols differ only in wire representation (most notably
//! nulls), the [`Frame`] variant is canonical and the [`crate::encode`] step
//! chooses the right bytes for the target [`RespVersion`].
//!
//! Reference: <https://redis.io/docs/latest/develop/reference/protocol-spec/>

use bytes::Bytes;

/// The wire protocol version negotiated for a connection.
///
/// A connection starts in [`RespVersion::V2`] and is upgraded to
/// [`RespVersion::V3`] when the client sends `HELLO 3`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RespVersion {
    /// RESP2 — the classic protocol every Redis client speaks.
    #[default]
    V2,
    /// RESP3 — adds typed replies (maps, sets, doubles, booleans, push, …).
    V3,
}

impl RespVersion {
    /// The integer as it appears in `HELLO <proto>` and the `proto` reply field.
    pub fn as_i64(self) -> i64 {
        match self {
            RespVersion::V2 => 2,
            RespVersion::V3 => 3,
        }
    }

    /// Parse from the integer used by `HELLO`. Returns `None` for anything but
    /// `2` or `3`.
    pub fn from_i64(v: i64) -> Option<Self> {
        match v {
            2 => Some(RespVersion::V2),
            3 => Some(RespVersion::V3),
            _ => None,
        }
    }
}

/// A single RESP value.
///
/// This models the union of RESP2 and RESP3. Encoding a RESP3-only variant while
/// targeting [`RespVersion::V2`] is the caller's responsibility (the server
/// builds version-appropriate replies); the encoder does not silently downgrade
/// aggregate types, it only picks the correct null representation.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    // ---- shared between RESP2 and RESP3 ----
    /// Simple string: `+OK\r\n`. Must not contain CR or LF.
    Simple(String),
    /// Error: `-ERR message\r\n`. Must not contain CR or LF.
    Error(String),
    /// Integer: `:1000\r\n`.
    Integer(i64),
    /// Bulk string: `$5\r\nhello\r\n`. Binary-safe.
    Bulk(Bytes),
    /// Array: `*2\r\n...`.
    Array(Vec<Frame>),
    /// Null. RESP2 wire form is `$-1\r\n` (null bulk); RESP3 is `_\r\n`.
    /// A decoded `*-1\r\n` (null array) also maps here.
    Null,

    // ---- RESP3 only ----
    /// Boolean: `#t\r\n` / `#f\r\n`.
    Boolean(bool),
    /// Double: `,3.14\r\n`, plus `,inf\r\n` / `,-inf\r\n` / `,nan\r\n`.
    Double(f64),
    /// Big number: `(3492890328409238509324850943850943825024385\r\n`.
    BigNumber(String),
    /// Bulk error: `!21\r\nSYNTAX invalid syntax\r\n`.
    BulkError(String),
    /// Verbatim string: `=15\r\ntxt:Some string\r\n` (3-byte format + `:` + data).
    Verbatim {
        /// The 3-byte format hint (e.g. `txt`, `mkd`).
        format: [u8; 3],
        /// The payload bytes after the format prefix.
        data: Bytes,
    },
    /// Map: `%2\r\n<k1><v1><k2><v2>`.
    Map(Vec<(Frame, Frame)>),
    /// Set: `~3\r\n...`.
    Set(Vec<Frame>),
    /// Out-of-band push: `>3\r\n...`.
    Push(Vec<Frame>),
}

impl Frame {
    /// A `+OK` simple string.
    pub fn ok() -> Frame {
        Frame::Simple("OK".to_string())
    }

    /// A simple string.
    pub fn simple(s: impl Into<String>) -> Frame {
        Frame::Simple(s.into())
    }

    /// An error frame.
    pub fn error(s: impl Into<String>) -> Frame {
        Frame::Error(s.into())
    }

    /// A bulk string from any byte source.
    pub fn bulk(b: impl Into<Bytes>) -> Frame {
        Frame::Bulk(b.into())
    }

    /// The canonical null.
    pub fn null() -> Frame {
        Frame::Null
    }

    /// If this frame is a bulk string, borrow its bytes.
    pub fn as_bulk(&self) -> Option<&Bytes> {
        match self {
            Frame::Bulk(b) => Some(b),
            _ => None,
        }
    }
}

impl From<&str> for Frame {
    fn from(s: &str) -> Self {
        Frame::Bulk(Bytes::copy_from_slice(s.as_bytes()))
    }
}

impl From<i64> for Frame {
    fn from(n: i64) -> Self {
        Frame::Integer(n)
    }
}
