//! Sans-IO, incremental RESP decoder.
//!
//! The decoder is **streaming**: it works on a growing byte buffer and returns
//! `Ok(None)` when a complete frame is not yet available, without consuming any
//! bytes. This is what makes fragmented reads (a frame split across many TCP
//! segments) transparent — the caller just keeps appending and re-calling.
//!
//! Two entry points:
//! - [`decode`] parses one arbitrary RESP value ([`Frame`]) — used for replies /
//!   general RESP streams.
//! - [`decode_command`] parses a client *request*, which is either a RESP array
//!   of bulk strings or an inline command. This is what the server read loop
//!   uses.
//!
//! All length-prefixed reads are bounded by [`Limits`] to keep a hostile or
//! buggy peer from triggering unbounded allocation.

use bytes::{Bytes, BytesMut};

use crate::error::ProtocolError;
use crate::frame::Frame;

/// Safety bounds applied while decoding.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Max length of a single bulk / verbatim / bulk-error payload.
    pub max_bulk_len: usize,
    /// Max element count of an aggregate (array / set / push, or pairs*2 for map)
    /// and of a multibulk request.
    pub max_array_len: usize,
    /// Max length of an inline command line.
    pub max_inline_len: usize,
    /// Max nesting depth of aggregates (guards against stack exhaustion).
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            // Mirrors Redis' `proto-max-bulk-len` default (512 MiB).
            max_bulk_len: 512 * 1024 * 1024,
            // Mirrors Redis' hard multibulk element cap (1M).
            max_array_len: 1024 * 1024,
            // Mirrors Redis' `PROTO_INLINE_MAX_SIZE` (64 KiB).
            max_inline_len: 64 * 1024,
            max_depth: 128,
        }
    }
}

/// Outcome of an internal parse step: either a value with the number of bytes it
/// consumed, or "need more data" (`None`).
type Parsed<T> = Result<Option<(T, usize)>, ProtocolError>;

/// Decode one RESP value from the front of `buf`.
///
/// On success the consumed bytes are removed from `buf`. Returns `Ok(None)` if
/// `buf` does not yet hold a complete frame (nothing is consumed).
pub fn decode(buf: &mut BytesMut, limits: &Limits) -> Result<Option<Frame>, ProtocolError> {
    match parse_frame(&buf[..], 0, limits, 0)? {
        Some((frame, consumed)) => {
            let _ = buf.split_to(consumed);
            Ok(Some(frame))
        }
        None => Ok(None),
    }
}

/// Decode one client request (RESP multibulk **or** inline) from `buf`.
///
/// Returns the command as a vector of argument byte-strings. An empty vector
/// (e.g. a blank inline line or `*0`) is returned as `Some(vec![])`; callers
/// typically treat that as "no command, keep reading".
pub fn decode_command(
    buf: &mut BytesMut,
    limits: &Limits,
) -> Result<Option<Vec<Bytes>>, ProtocolError> {
    if buf.is_empty() {
        return Ok(None);
    }
    if buf[0] == b'*' {
        match parse_multibulk(&buf[..], limits)? {
            Some((args, consumed)) => {
                let _ = buf.split_to(consumed);
                Ok(Some(args))
            }
            None => Ok(None),
        }
    } else {
        // Inline command.
        match read_line(&buf[..], 0)? {
            Some((line, consumed)) => {
                if line.len() > limits.max_inline_len {
                    return Err(ProtocolError::LimitExceeded(format!(
                        "inline command of {} bytes exceeds limit {}",
                        line.len(),
                        limits.max_inline_len
                    )));
                }
                let args = split_inline(line)?;
                let _ = buf.split_to(consumed);
                Ok(Some(args))
            }
            None => {
                // No CRLF yet. Reject early if the partial line is already too big
                // rather than buffering unboundedly.
                if buf.len() > limits.max_inline_len {
                    return Err(ProtocolError::LimitExceeded(format!(
                        "inline command exceeds limit {} before end of line",
                        limits.max_inline_len
                    )));
                }
                Ok(None)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Internal parser
// ---------------------------------------------------------------------------

fn parse_frame(input: &[u8], from: usize, limits: &Limits, depth: usize) -> Parsed<Frame> {
    if depth > limits.max_depth {
        return Err(ProtocolError::malformed(format!(
            "nesting deeper than {} levels",
            limits.max_depth
        )));
    }
    if from >= input.len() {
        return Ok(None);
    }
    let type_byte = input[from];
    let body = from + 1;
    match type_byte {
        b'+' => text_line(input, body, Frame::Simple),
        b'-' => text_line(input, body, Frame::Error),
        b'(' => text_line(input, body, Frame::BigNumber),
        b':' => parse_integer(input, body),
        b'_' => parse_null(input, body),
        b'#' => parse_boolean(input, body),
        b',' => parse_double(input, body),
        b'$' => parse_bulk(input, body, limits, false),
        b'!' => parse_bulk(input, body, limits, true),
        b'=' => parse_verbatim(input, body, limits),
        b'*' => parse_aggregate(input, body, limits, depth, Aggregate::Array),
        b'~' => parse_aggregate(input, body, limits, depth, Aggregate::Set),
        b'>' => parse_aggregate(input, body, limits, depth, Aggregate::Push),
        b'%' => parse_map(input, body, limits, depth),
        other => Err(ProtocolError::malformed(format!(
            "unknown RESP type byte 0x{other:02x}"
        ))),
    }
}

/// A simple text line frame (`+`, `-`, `(`), validated as UTF-8.
fn text_line(input: &[u8], from: usize, make: impl FnOnce(String) -> Frame) -> Parsed<Frame> {
    match read_line(input, from)? {
        Some((line, next)) => {
            let s = str_from(line)?;
            Ok(Some((make(s.to_string()), next)))
        }
        None => Ok(None),
    }
}

fn parse_integer(input: &[u8], from: usize) -> Parsed<Frame> {
    match read_line(input, from)? {
        Some((line, next)) => Ok(Some((Frame::Integer(parse_i64(line)?), next))),
        None => Ok(None),
    }
}

fn parse_null(input: &[u8], from: usize) -> Parsed<Frame> {
    match read_line(input, from)? {
        Some((line, next)) => {
            if !line.is_empty() {
                return Err(ProtocolError::malformed("null (_) must have empty body"));
            }
            Ok(Some((Frame::Null, next)))
        }
        None => Ok(None),
    }
}

fn parse_boolean(input: &[u8], from: usize) -> Parsed<Frame> {
    match read_line(input, from)? {
        Some((line, next)) => {
            let v = match line {
                b"t" => true,
                b"f" => false,
                _ => return Err(ProtocolError::malformed("boolean must be #t or #f")),
            };
            Ok(Some((Frame::Boolean(v), next)))
        }
        None => Ok(None),
    }
}

fn parse_double(input: &[u8], from: usize) -> Parsed<Frame> {
    match read_line(input, from)? {
        Some((line, next)) => {
            let s = str_from(line)?;
            let d = match s {
                "inf" | "+inf" => f64::INFINITY,
                "-inf" => f64::NEG_INFINITY,
                "nan" | "-nan" => f64::NAN,
                _ => s
                    .parse::<f64>()
                    .map_err(|_| ProtocolError::malformed(format!("invalid double '{s}'")))?,
            };
            Ok(Some((Frame::Double(d), next)))
        }
        None => Ok(None),
    }
}

/// Parse a `$` bulk string or `!` bulk error. `is_error` selects the frame kind.
fn parse_bulk(input: &[u8], from: usize, limits: &Limits, is_error: bool) -> Parsed<Frame> {
    let (len, after_hdr) = match read_line(input, from)? {
        Some((line, next)) => (parse_i64(line)?, next),
        None => return Ok(None),
    };
    // `$-1` is the RESP2 null bulk (only valid for `$`, not `!`).
    if len == -1 && !is_error {
        return Ok(Some((Frame::Null, after_hdr)));
    }
    if len < 0 {
        return Err(ProtocolError::malformed(format!(
            "negative bulk length {len}"
        )));
    }
    let len = len as usize;
    if len > limits.max_bulk_len {
        return Err(ProtocolError::LimitExceeded(format!(
            "bulk length {len} exceeds limit {}",
            limits.max_bulk_len
        )));
    }
    // Need the payload plus the trailing CRLF.
    let end = after_hdr + len;
    if input.len() < end + 2 {
        return Ok(None);
    }
    if &input[end..end + 2] != b"\r\n" {
        return Err(ProtocolError::malformed("bulk not terminated by CRLF"));
    }
    let payload = &input[after_hdr..end];
    let frame = if is_error {
        Frame::BulkError(str_from(payload)?.to_string())
    } else {
        Frame::Bulk(Bytes::copy_from_slice(payload))
    };
    Ok(Some((frame, end + 2)))
}

fn parse_verbatim(input: &[u8], from: usize, limits: &Limits) -> Parsed<Frame> {
    let (len, after_hdr) = match read_line(input, from)? {
        Some((line, next)) => (parse_i64(line)?, next),
        None => return Ok(None),
    };
    if len < 4 {
        return Err(ProtocolError::malformed(
            "verbatim string too short (need 3-byte format + ':')",
        ));
    }
    let len = len as usize;
    if len > limits.max_bulk_len {
        return Err(ProtocolError::LimitExceeded(format!(
            "verbatim length {len} exceeds limit {}",
            limits.max_bulk_len
        )));
    }
    let end = after_hdr + len;
    if input.len() < end + 2 {
        return Ok(None);
    }
    if &input[end..end + 2] != b"\r\n" {
        return Err(ProtocolError::malformed("verbatim not terminated by CRLF"));
    }
    let body = &input[after_hdr..end];
    if body[3] != b':' {
        return Err(ProtocolError::malformed(
            "verbatim format not followed by ':'",
        ));
    }
    let format = [body[0], body[1], body[2]];
    let data = Bytes::copy_from_slice(&body[4..]);
    Ok(Some((Frame::Verbatim { format, data }, end + 2)))
}

/// Which linear aggregate we're parsing (all share the `*N`-style layout).
enum Aggregate {
    Array,
    Set,
    Push,
}

fn parse_aggregate(
    input: &[u8],
    from: usize,
    limits: &Limits,
    depth: usize,
    kind: Aggregate,
) -> Parsed<Frame> {
    let (count, mut pos) = match read_line(input, from)? {
        Some((line, next)) => (parse_i64(line)?, next),
        None => return Ok(None),
    };
    // `*-1` is the RESP2 null array.
    if count == -1 {
        if let Aggregate::Array = kind {
            return Ok(Some((Frame::Null, pos)));
        }
        return Err(ProtocolError::malformed("negative aggregate length"));
    }
    if count < 0 {
        return Err(ProtocolError::malformed("negative aggregate length"));
    }
    let count = count as usize;
    if count > limits.max_array_len {
        return Err(ProtocolError::LimitExceeded(format!(
            "aggregate length {count} exceeds limit {}",
            limits.max_array_len
        )));
    }
    let mut items = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        match parse_frame(input, pos, limits, depth + 1)? {
            Some((frame, next)) => {
                items.push(frame);
                pos = next;
            }
            None => return Ok(None),
        }
    }
    let frame = match kind {
        Aggregate::Array => Frame::Array(items),
        Aggregate::Set => Frame::Set(items),
        Aggregate::Push => Frame::Push(items),
    };
    Ok(Some((frame, pos)))
}

fn parse_map(input: &[u8], from: usize, limits: &Limits, depth: usize) -> Parsed<Frame> {
    let (count, mut pos) = match read_line(input, from)? {
        Some((line, next)) => (parse_i64(line)?, next),
        None => return Ok(None),
    };
    if count < 0 {
        return Err(ProtocolError::malformed("negative map length"));
    }
    let count = count as usize;
    if count > limits.max_array_len {
        return Err(ProtocolError::LimitExceeded(format!(
            "map length {count} exceeds limit {}",
            limits.max_array_len
        )));
    }
    let mut pairs = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        let key = match parse_frame(input, pos, limits, depth + 1)? {
            Some((frame, next)) => {
                pos = next;
                frame
            }
            None => return Ok(None),
        };
        let val = match parse_frame(input, pos, limits, depth + 1)? {
            Some((frame, next)) => {
                pos = next;
                frame
            }
            None => return Ok(None),
        };
        pairs.push((key, val));
    }
    Ok(Some((Frame::Map(pairs), pos)))
}

/// Parse a `*N`-prefixed request whose elements must all be bulk strings.
fn parse_multibulk(input: &[u8], limits: &Limits) -> Parsed<Vec<Bytes>> {
    debug_assert_eq!(input.first(), Some(&b'*'));
    let (count, mut pos) = match read_line(input, 1)? {
        Some((line, next)) => (parse_i64(line)?, next),
        None => return Ok(None),
    };
    if count <= 0 {
        // `*0` / `*-1` → empty command; the caller skips it.
        return Ok(Some((Vec::new(), pos)));
    }
    let count = count as usize;
    if count > limits.max_array_len {
        return Err(ProtocolError::LimitExceeded(format!(
            "multibulk length {count} exceeds limit {}",
            limits.max_array_len
        )));
    }
    let mut args = Vec::with_capacity(count.min(1024));
    for _ in 0..count {
        if pos >= input.len() {
            return Ok(None);
        }
        if input[pos] != b'$' {
            return Err(ProtocolError::malformed(format!(
                "expected '$' in multibulk, got 0x{:02x}",
                input[pos]
            )));
        }
        let (len, after_hdr) = match read_line(input, pos + 1)? {
            Some((line, next)) => (parse_i64(line)?, next),
            None => return Ok(None),
        };
        if len < 0 {
            return Err(ProtocolError::malformed(
                "null bulk string not allowed in a request",
            ));
        }
        let len = len as usize;
        if len > limits.max_bulk_len {
            return Err(ProtocolError::LimitExceeded(format!(
                "bulk length {len} exceeds limit {}",
                limits.max_bulk_len
            )));
        }
        let end = after_hdr + len;
        if input.len() < end + 2 {
            return Ok(None);
        }
        if &input[end..end + 2] != b"\r\n" {
            return Err(ProtocolError::malformed("bulk not terminated by CRLF"));
        }
        args.push(Bytes::copy_from_slice(&input[after_hdr..end]));
        pos = end + 2;
    }
    Ok(Some((args, pos)))
}

// ---------------------------------------------------------------------------
// Line / integer helpers
// ---------------------------------------------------------------------------

/// Find the next `\r\n` at or after `from`, returning the line (without CRLF)
/// and the position just past it. `Ok(None)` means the terminator isn't in the
/// buffer yet.
fn read_line(input: &[u8], from: usize) -> Parsed<&[u8]> {
    if from > input.len() {
        return Ok(None);
    }
    let mut i = from;
    while i + 1 < input.len() {
        if input[i] == b'\r' && input[i + 1] == b'\n' {
            return Ok(Some((&input[from..i], i + 2)));
        }
        i += 1;
    }
    Ok(None)
}

fn str_from(bytes: &[u8]) -> Result<&str, ProtocolError> {
    std::str::from_utf8(bytes).map_err(|_| ProtocolError::malformed("invalid UTF-8 in text line"))
}

fn parse_i64(bytes: &[u8]) -> Result<i64, ProtocolError> {
    let s = str_from(bytes)?;
    s.parse::<i64>()
        .map_err(|_| ProtocolError::malformed(format!("invalid integer '{s}'")))
}

// ---------------------------------------------------------------------------
// Inline command tokenizer (mirrors Redis' sdssplitargs)
// ---------------------------------------------------------------------------

/// Split an inline command line into arguments, honoring single/double quotes
/// and the escape sequences Redis supports. Errors on unbalanced quotes.
fn split_inline(line: &[u8]) -> Result<Vec<Bytes>, ProtocolError> {
    let mut args = Vec::new();
    let mut i = 0;
    let n = line.len();
    while i < n {
        // Skip leading whitespace.
        while i < n && is_space(line[i]) {
            i += 1;
        }
        if i >= n {
            break;
        }
        let mut cur: Vec<u8> = Vec::new();
        match line[i] {
            b'"' => {
                i += 1;
                loop {
                    if i >= n {
                        return Err(ProtocolError::InlineSyntax(
                            "unbalanced double quotes".to_string(),
                        ));
                    }
                    match line[i] {
                        b'"' => {
                            i += 1;
                            // Closing quote must be followed by space or EOL.
                            if i < n && !is_space(line[i]) {
                                return Err(ProtocolError::InlineSyntax(
                                    "closing quote not followed by space".to_string(),
                                ));
                            }
                            break;
                        }
                        b'\\' if i + 1 < n => {
                            i += 1;
                            let (byte, adv) = unescape_double(&line[i..]);
                            cur.push(byte);
                            i += adv;
                        }
                        c => {
                            cur.push(c);
                            i += 1;
                        }
                    }
                }
            }
            b'\'' => {
                i += 1;
                loop {
                    if i >= n {
                        return Err(ProtocolError::InlineSyntax(
                            "unbalanced single quotes".to_string(),
                        ));
                    }
                    match line[i] {
                        b'\'' => {
                            i += 1;
                            if i < n && !is_space(line[i]) {
                                return Err(ProtocolError::InlineSyntax(
                                    "closing quote not followed by space".to_string(),
                                ));
                            }
                            break;
                        }
                        b'\\' if i + 1 < n && line[i + 1] == b'\'' => {
                            cur.push(b'\'');
                            i += 2;
                        }
                        c => {
                            cur.push(c);
                            i += 1;
                        }
                    }
                }
            }
            _ => {
                while i < n && !is_space(line[i]) {
                    cur.push(line[i]);
                    i += 1;
                }
            }
        }
        args.push(Bytes::from(cur));
    }
    Ok(args)
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
}

/// Decode one escape sequence inside a double-quoted token. `rest` starts at the
/// char after the backslash. Returns the decoded byte and how many input bytes
/// were consumed from `rest`.
fn unescape_double(rest: &[u8]) -> (u8, usize) {
    match rest[0] {
        b'x' if rest.len() >= 3 && is_hex(rest[1]) && is_hex(rest[2]) => {
            let hi = hex_val(rest[1]);
            let lo = hex_val(rest[2]);
            ((hi << 4) | lo, 3)
        }
        b'n' => (b'\n', 1),
        b'r' => (b'\r', 1),
        b't' => (b'\t', 1),
        b'b' => (0x08, 1),
        b'a' => (0x07, 1),
        other => (other, 1),
    }
}

fn is_hex(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

fn hex_val(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(bytes: &[u8]) -> Frame {
        let mut buf = BytesMut::from(bytes);
        let frame = decode(&mut buf, &Limits::default())
            .expect("decode ok")
            .expect("frame present");
        assert!(buf.is_empty(), "decoder left {} bytes", buf.len());
        frame
    }

    fn cmd(bytes: &[u8]) -> Vec<Bytes> {
        let mut buf = BytesMut::from(bytes);
        decode_command(&mut buf, &Limits::default())
            .expect("decode ok")
            .expect("command present")
    }

    // ---- RESP2 value types ----

    #[test]
    fn decode_simple_string() {
        assert_eq!(d(b"+OK\r\n"), Frame::Simple("OK".into()));
    }

    #[test]
    fn decode_error() {
        assert_eq!(d(b"-ERR bad\r\n"), Frame::Error("ERR bad".into()));
    }

    #[test]
    fn decode_integer() {
        assert_eq!(d(b":1000\r\n"), Frame::Integer(1000));
        assert_eq!(d(b":-42\r\n"), Frame::Integer(-42));
    }

    #[test]
    fn decode_bulk_string() {
        assert_eq!(
            d(b"$5\r\nhello\r\n"),
            Frame::Bulk(Bytes::from_static(b"hello"))
        );
        assert_eq!(d(b"$0\r\n\r\n"), Frame::Bulk(Bytes::new()));
    }

    #[test]
    fn decode_bulk_is_binary_safe() {
        assert_eq!(
            d(b"$3\r\n\x00\r\n\r\n"),
            Frame::Bulk(Bytes::from_static(b"\x00\r\n"))
        );
    }

    #[test]
    fn decode_null_bulk_and_null_array_map_to_null() {
        assert_eq!(d(b"$-1\r\n"), Frame::Null);
        assert_eq!(d(b"*-1\r\n"), Frame::Null);
    }

    #[test]
    fn decode_array() {
        assert_eq!(
            d(b"*2\r\n$3\r\nfoo\r\n:7\r\n"),
            Frame::Array(vec![
                Frame::Bulk(Bytes::from_static(b"foo")),
                Frame::Integer(7)
            ])
        );
        assert_eq!(d(b"*0\r\n"), Frame::Array(vec![]));
    }

    #[test]
    fn decode_nested_array() {
        assert_eq!(
            d(b"*2\r\n*1\r\n:1\r\n:2\r\n"),
            Frame::Array(vec![
                Frame::Array(vec![Frame::Integer(1)]),
                Frame::Integer(2)
            ])
        );
    }

    // ---- RESP3 value types ----

    #[test]
    fn decode_resp3_null() {
        assert_eq!(d(b"_\r\n"), Frame::Null);
    }

    #[test]
    fn decode_boolean() {
        assert_eq!(d(b"#t\r\n"), Frame::Boolean(true));
        assert_eq!(d(b"#f\r\n"), Frame::Boolean(false));
    }

    #[test]
    fn decode_double() {
        assert_eq!(d(b",3.25\r\n"), Frame::Double(3.25));
        assert_eq!(d(b",inf\r\n"), Frame::Double(f64::INFINITY));
        assert_eq!(d(b",-inf\r\n"), Frame::Double(f64::NEG_INFINITY));
        match d(b",nan\r\n") {
            Frame::Double(x) => assert!(x.is_nan()),
            other => panic!("expected double, got {other:?}"),
        }
    }

    #[test]
    fn decode_big_number() {
        assert_eq!(
            d(b"(3492890328409238509324850943850943825024385\r\n"),
            Frame::BigNumber("3492890328409238509324850943850943825024385".into())
        );
    }

    #[test]
    fn decode_bulk_error() {
        assert_eq!(
            d(b"!21\r\nSYNTAX invalid syntax\r\n"),
            Frame::BulkError("SYNTAX invalid syntax".into())
        );
    }

    #[test]
    fn decode_verbatim() {
        assert_eq!(
            d(b"=15\r\ntxt:Some string\r\n"),
            Frame::Verbatim {
                format: *b"txt",
                data: Bytes::from_static(b"Some string"),
            }
        );
    }

    #[test]
    fn decode_map() {
        assert_eq!(
            d(b"%1\r\n$3\r\nkey\r\n:5\r\n"),
            Frame::Map(vec![(
                Frame::Bulk(Bytes::from_static(b"key")),
                Frame::Integer(5)
            )])
        );
    }

    #[test]
    fn decode_set_and_push() {
        assert_eq!(
            d(b"~2\r\n:1\r\n:2\r\n"),
            Frame::Set(vec![Frame::Integer(1), Frame::Integer(2)])
        );
        assert_eq!(
            d(b">1\r\n$3\r\nmsg\r\n"),
            Frame::Push(vec![Frame::Bulk(Bytes::from_static(b"msg"))])
        );
    }

    // ---- streaming / fragmented reads ----

    #[test]
    fn incomplete_returns_none_without_consuming() {
        let limits = Limits::default();
        // Header present, payload missing.
        let mut buf = BytesMut::from(&b"$5\r\nhel"[..]);
        assert_eq!(decode(&mut buf, &limits).unwrap(), None);
        assert_eq!(buf.len(), 7, "nothing should be consumed");
        // Complete it.
        buf.extend_from_slice(b"lo\r\n");
        assert_eq!(
            decode(&mut buf, &limits).unwrap(),
            Some(Frame::Bulk(Bytes::from_static(b"hello")))
        );
    }

    #[test]
    fn missing_final_crlf_is_incomplete() {
        let limits = Limits::default();
        let mut buf = BytesMut::from(&b"$5\r\nhello"[..]);
        assert_eq!(decode(&mut buf, &limits).unwrap(), None);
    }

    #[test]
    fn two_frames_decode_sequentially() {
        let limits = Limits::default();
        let mut buf = BytesMut::from(&b"+A\r\n:2\r\n"[..]);
        assert_eq!(
            decode(&mut buf, &limits).unwrap(),
            Some(Frame::Simple("A".into()))
        );
        assert_eq!(decode(&mut buf, &limits).unwrap(), Some(Frame::Integer(2)));
        assert_eq!(decode(&mut buf, &limits).unwrap(), None);
    }

    // ---- errors ----

    #[test]
    fn unknown_type_byte_errors() {
        let mut buf = BytesMut::from(&b"@nope\r\n"[..]);
        assert!(matches!(
            decode(&mut buf, &Limits::default()),
            Err(ProtocolError::Malformed(_))
        ));
    }

    #[test]
    fn bad_integer_errors() {
        let mut buf = BytesMut::from(&b":abc\r\n"[..]);
        assert!(matches!(
            decode(&mut buf, &Limits::default()),
            Err(ProtocolError::Malformed(_))
        ));
    }

    #[test]
    fn bulk_over_limit_errors() {
        let limits = Limits {
            max_bulk_len: 4,
            ..Limits::default()
        };
        let mut buf = BytesMut::from(&b"$10\r\n"[..]);
        assert!(matches!(
            decode(&mut buf, &limits),
            Err(ProtocolError::LimitExceeded(_))
        ));
    }

    #[test]
    fn array_over_limit_errors() {
        let limits = Limits {
            max_array_len: 2,
            ..Limits::default()
        };
        let mut buf = BytesMut::from(&b"*5\r\n"[..]);
        assert!(matches!(
            decode(&mut buf, &limits),
            Err(ProtocolError::LimitExceeded(_))
        ));
    }

    #[test]
    fn depth_over_limit_errors() {
        let limits = Limits {
            max_depth: 2,
            ..Limits::default()
        };
        let mut buf = BytesMut::from(&b"*1\r\n*1\r\n*1\r\n*1\r\n:1\r\n"[..]);
        assert!(matches!(
            decode(&mut buf, &limits),
            Err(ProtocolError::Malformed(_))
        ));
    }

    // ---- command decoding: multibulk ----

    #[test]
    fn decode_multibulk_command() {
        assert_eq!(
            cmd(b"*2\r\n$3\r\nGET\r\n$3\r\nfoo\r\n"),
            vec![Bytes::from_static(b"GET"), Bytes::from_static(b"foo")]
        );
    }

    #[test]
    fn decode_empty_multibulk_is_empty_args() {
        assert_eq!(cmd(b"*0\r\n"), Vec::<Bytes>::new());
    }

    #[test]
    fn multibulk_null_element_errors() {
        let mut buf = BytesMut::from(&b"*1\r\n$-1\r\n"[..]);
        assert!(matches!(
            decode_command(&mut buf, &Limits::default()),
            Err(ProtocolError::Malformed(_))
        ));
    }

    #[test]
    fn multibulk_non_bulk_element_errors() {
        let mut buf = BytesMut::from(&b"*1\r\n:5\r\n"[..]);
        assert!(matches!(
            decode_command(&mut buf, &Limits::default()),
            Err(ProtocolError::Malformed(_))
        ));
    }

    // ---- command decoding: inline ----

    #[test]
    fn decode_inline_command() {
        assert_eq!(cmd(b"PING\r\n"), vec![Bytes::from_static(b"PING")]);
        assert_eq!(
            cmd(b"SET foo bar\r\n"),
            vec![
                Bytes::from_static(b"SET"),
                Bytes::from_static(b"foo"),
                Bytes::from_static(b"bar"),
            ]
        );
    }

    #[test]
    fn decode_inline_blank_line_is_empty() {
        assert_eq!(cmd(b"\r\n"), Vec::<Bytes>::new());
        assert_eq!(cmd(b"   \r\n"), Vec::<Bytes>::new());
    }

    #[test]
    fn decode_inline_double_quotes_with_escapes() {
        assert_eq!(
            cmd(b"SET k \"a b\\tc\\x41\"\r\n"),
            vec![
                Bytes::from_static(b"SET"),
                Bytes::from_static(b"k"),
                Bytes::from_static(b"a b\tcA"),
            ]
        );
    }

    #[test]
    fn decode_inline_single_quotes() {
        assert_eq!(
            cmd(b"SET k 'a b'\r\n"),
            vec![
                Bytes::from_static(b"SET"),
                Bytes::from_static(b"k"),
                Bytes::from_static(b"a b"),
            ]
        );
    }

    #[test]
    fn decode_inline_unbalanced_quote_errors() {
        let mut buf = BytesMut::from(&b"SET k \"oops\r\n"[..]);
        assert!(matches!(
            decode_command(&mut buf, &Limits::default()),
            Err(ProtocolError::InlineSyntax(_))
        ));
    }

    #[test]
    fn decode_inline_incomplete_returns_none() {
        let mut buf = BytesMut::from(&b"PING"[..]);
        assert_eq!(decode_command(&mut buf, &Limits::default()).unwrap(), None);
        assert_eq!(buf.len(), 4);
    }

    #[test]
    fn inline_over_limit_errors() {
        let limits = Limits {
            max_inline_len: 4,
            ..Limits::default()
        };
        let mut buf = BytesMut::from(&b"AAAAAAAA\r\n"[..]);
        assert!(matches!(
            decode_command(&mut buf, &limits),
            Err(ProtocolError::LimitExceeded(_))
        ));
    }
}
