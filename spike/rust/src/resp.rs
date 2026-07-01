//! Minimal RESP2 wire protocol: request parsing and reply serialization.
//! Enough to talk to `redis-cli` and `redis-py` for the supported commands.

use std::io::{self, BufRead, Write};

/// A reply value in the RESP2 type system.
pub enum Reply {
    Simple(String),
    Error(String),
    Int(i64),
    Bulk(Option<Vec<u8>>),
    Array(Vec<Reply>),
}

impl Reply {
    pub fn ok() -> Reply {
        Reply::Simple("OK".to_string())
    }

    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        match self {
            Reply::Simple(s) => write!(w, "+{}\r\n", s),
            Reply::Error(s) => write!(w, "-{}\r\n", s),
            Reply::Int(n) => write!(w, ":{}\r\n", n),
            Reply::Bulk(None) => write!(w, "$-1\r\n"),
            Reply::Bulk(Some(bytes)) => {
                write!(w, "${}\r\n", bytes.len())?;
                w.write_all(bytes)?;
                w.write_all(b"\r\n")
            }
            Reply::Array(items) => {
                write!(w, "*{}\r\n", items.len())?;
                for item in items {
                    item.write_to(w)?;
                }
                Ok(())
            }
        }
    }
}

/// Read one client command, returning its arguments as raw byte vectors.
/// Returns Ok(None) on a clean EOF. Supports both RESP arrays (the normal
/// client encoding) and simple inline commands (handy for manual `nc` testing).
pub fn read_command<R: BufRead>(reader: &mut R) -> io::Result<Option<Vec<Vec<u8>>>> {
    let mut line = Vec::new();
    if read_line(reader, &mut line)? == 0 {
        return Ok(None);
    }
    if line.is_empty() {
        return Ok(Some(Vec::new()));
    }

    if line[0] == b'*' {
        let count: i64 = parse_int(&line[1..])?;
        if count <= 0 {
            return Ok(Some(Vec::new()));
        }
        let mut args = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let mut hdr = Vec::new();
            if read_line(reader, &mut hdr)? == 0 {
                return Ok(None);
            }
            if hdr.is_empty() || hdr[0] != b'$' {
                return Err(proto_err("expected bulk string"));
            }
            let len: i64 = parse_int(&hdr[1..])?;
            if len < 0 {
                args.push(Vec::new());
                continue;
            }
            let mut buf = vec![0u8; len as usize + 2]; // value + CRLF
            read_exact(reader, &mut buf)?;
            buf.truncate(len as usize);
            args.push(buf);
        }
        Ok(Some(args))
    } else {
        // Inline command: split on whitespace.
        let args = line
            .split(|b| *b == b' ' || *b == b'\t')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_vec())
            .collect();
        Ok(Some(args))
    }
}

/// Read up to and including a `\n`, stripping the trailing CRLF/LF.
/// Returns the number of bytes read (0 on EOF).
fn read_line<R: BufRead>(reader: &mut R, out: &mut Vec<u8>) -> io::Result<usize> {
    let mut raw = Vec::new();
    let n = reader.read_until(b'\n', &mut raw)?;
    if n == 0 {
        return Ok(0);
    }
    while raw.last() == Some(&b'\n') || raw.last() == Some(&b'\r') {
        raw.pop();
    }
    *out = raw;
    Ok(n)
}

fn read_exact<R: BufRead>(reader: &mut R, buf: &mut [u8]) -> io::Result<()> {
    io::Read::read_exact(reader, buf)
}

fn parse_int(bytes: &[u8]) -> io::Result<i64> {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .ok_or_else(|| proto_err("invalid integer"))
}

fn proto_err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}
