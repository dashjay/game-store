//! Sans-IO RESP encoder.
//!
//! [`encode`] serializes a [`Frame`] into a byte buffer for a given
//! [`RespVersion`]. It never performs I/O; callers own the buffer and flush it
//! however they like (the tokio [`crate::codec`] adapter does this for you).

use bytes::{BufMut, BytesMut};

use crate::frame::{Frame, RespVersion};

/// Append the wire encoding of `frame` to `out`, using `version`'s conventions
/// where the two protocols differ (currently only the null representation).
pub fn encode(frame: &Frame, version: RespVersion, out: &mut BytesMut) {
    match frame {
        Frame::Simple(s) => {
            out.put_u8(b'+');
            out.put_slice(s.as_bytes());
            put_crlf(out);
        }
        Frame::Error(s) => {
            out.put_u8(b'-');
            out.put_slice(s.as_bytes());
            put_crlf(out);
        }
        Frame::Integer(n) => {
            out.put_u8(b':');
            put_int(out, *n);
            put_crlf(out);
        }
        Frame::Bulk(b) => {
            out.put_u8(b'$');
            put_int(out, b.len() as i64);
            put_crlf(out);
            out.put_slice(b);
            put_crlf(out);
        }
        Frame::Null => match version {
            // RESP2 has no dedicated null; the null bulk string is canonical.
            RespVersion::V2 => out.put_slice(b"$-1\r\n"),
            RespVersion::V3 => out.put_slice(b"_\r\n"),
        },
        Frame::Array(items) => {
            out.put_u8(b'*');
            put_int(out, items.len() as i64);
            put_crlf(out);
            for item in items {
                encode(item, version, out);
            }
        }

        // ---- RESP3 ----
        Frame::Boolean(v) => {
            out.put_slice(if *v { b"#t\r\n" } else { b"#f\r\n" });
        }
        Frame::Double(d) => {
            out.put_u8(b',');
            put_double(out, *d);
            put_crlf(out);
        }
        Frame::BigNumber(s) => {
            out.put_u8(b'(');
            out.put_slice(s.as_bytes());
            put_crlf(out);
        }
        Frame::BulkError(s) => {
            out.put_u8(b'!');
            put_int(out, s.len() as i64);
            put_crlf(out);
            out.put_slice(s.as_bytes());
            put_crlf(out);
        }
        Frame::Verbatim { format, data } => {
            // Length covers the 3-byte format, the ':' separator and the data.
            out.put_u8(b'=');
            put_int(out, (3 + 1 + data.len()) as i64);
            put_crlf(out);
            out.put_slice(format);
            out.put_u8(b':');
            out.put_slice(data);
            put_crlf(out);
        }
        Frame::Map(pairs) => {
            out.put_u8(b'%');
            put_int(out, pairs.len() as i64);
            put_crlf(out);
            for (k, v) in pairs {
                encode(k, version, out);
                encode(v, version, out);
            }
        }
        Frame::Set(items) => {
            out.put_u8(b'~');
            put_int(out, items.len() as i64);
            put_crlf(out);
            for item in items {
                encode(item, version, out);
            }
        }
        Frame::Push(items) => {
            out.put_u8(b'>');
            put_int(out, items.len() as i64);
            put_crlf(out);
            for item in items {
                encode(item, version, out);
            }
        }
    }
}

/// Convenience: encode a single frame into a fresh [`BytesMut`].
pub fn encode_to_vec(frame: &Frame, version: RespVersion) -> BytesMut {
    let mut out = BytesMut::new();
    encode(frame, version, &mut out);
    out
}

fn put_crlf(out: &mut BytesMut) {
    out.put_slice(b"\r\n");
}

fn put_int(out: &mut BytesMut, n: i64) {
    // A dedicated integer formatter (itoa) would shave an allocation; deferred
    // to the I-07 performance pass. Correctness first.
    let mut buf = [0u8; 20];
    let s = format_i64(n, &mut buf);
    out.put_slice(s);
}

/// Format `n` into `buf` as ASCII decimal, returning the written slice.
/// `buf` must be at least 20 bytes (max length of i64::MIN incl. sign).
fn format_i64(n: i64, buf: &mut [u8; 20]) -> &[u8] {
    use std::io::{Cursor, Write as _};
    let mut cursor = Cursor::new(&mut buf[..]);
    write!(cursor, "{n}").expect("i64 always fits in 20 bytes");
    let len = cursor.position() as usize;
    &buf[..len]
}

/// Format a double the way RESP3 expects, with the special infinities/NaN.
fn put_double(out: &mut BytesMut, d: f64) {
    if d.is_nan() {
        out.put_slice(b"nan");
    } else if d.is_infinite() {
        out.put_slice(if d > 0.0 { b"inf" } else { b"-inf" });
    } else {
        // Rust's default float formatting is the shortest representation that
        // round-trips exactly through `f64::from_str`, which is what our decoder
        // uses — so encode/decode is lossless for finite values.
        let s = format!("{d}");
        out.put_slice(s.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn enc(frame: &Frame, version: RespVersion) -> Vec<u8> {
        encode_to_vec(frame, version).to_vec()
    }

    #[test]
    fn encode_simple_and_error_and_int() {
        assert_eq!(enc(&Frame::simple("OK"), RespVersion::V2), b"+OK\r\n");
        assert_eq!(enc(&Frame::error("ERR x"), RespVersion::V2), b"-ERR x\r\n");
        assert_eq!(enc(&Frame::Integer(-9), RespVersion::V2), b":-9\r\n");
    }

    #[test]
    fn encode_bulk() {
        assert_eq!(
            enc(&Frame::Bulk(Bytes::from_static(b"hello")), RespVersion::V2),
            b"$5\r\nhello\r\n"
        );
        assert_eq!(
            enc(&Frame::Bulk(Bytes::new()), RespVersion::V2),
            b"$0\r\n\r\n"
        );
    }

    #[test]
    fn encode_null_is_version_specific() {
        assert_eq!(enc(&Frame::Null, RespVersion::V2), b"$-1\r\n");
        assert_eq!(enc(&Frame::Null, RespVersion::V3), b"_\r\n");
    }

    #[test]
    fn encode_array() {
        assert_eq!(
            enc(
                &Frame::Array(vec![Frame::Integer(1), Frame::simple("x")]),
                RespVersion::V2
            ),
            b"*2\r\n:1\r\n+x\r\n"
        );
    }

    #[test]
    fn encode_resp3_scalars() {
        assert_eq!(enc(&Frame::Boolean(true), RespVersion::V3), b"#t\r\n");
        assert_eq!(enc(&Frame::Boolean(false), RespVersion::V3), b"#f\r\n");
        assert_eq!(enc(&Frame::Double(3.25), RespVersion::V3), b",3.25\r\n");
        assert_eq!(
            enc(&Frame::Double(f64::INFINITY), RespVersion::V3),
            b",inf\r\n"
        );
        assert_eq!(
            enc(&Frame::Double(f64::NEG_INFINITY), RespVersion::V3),
            b",-inf\r\n"
        );
        assert_eq!(enc(&Frame::Double(f64::NAN), RespVersion::V3), b",nan\r\n");
    }

    #[test]
    fn encode_verbatim_length_covers_prefix() {
        assert_eq!(
            enc(
                &Frame::Verbatim {
                    format: *b"txt",
                    data: Bytes::from_static(b"Some string"),
                },
                RespVersion::V3
            ),
            b"=15\r\ntxt:Some string\r\n"
        );
    }

    #[test]
    fn encode_map_set_push() {
        assert_eq!(
            enc(
                &Frame::Map(vec![(Frame::simple("k"), Frame::Integer(1))]),
                RespVersion::V3
            ),
            b"%1\r\n+k\r\n:1\r\n"
        );
        assert_eq!(
            enc(&Frame::Set(vec![Frame::Integer(1)]), RespVersion::V3),
            b"~1\r\n:1\r\n"
        );
        assert_eq!(
            enc(&Frame::Push(vec![Frame::simple("m")]), RespVersion::V3),
            b">1\r\n+m\r\n"
        );
    }
}
