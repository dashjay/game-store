//! Minimal async RESP2 handling — just enough for I-01's PING/PONG smoke server.
//!
//! This is deliberately tiny and self-contained. The full, hardened sans-IO
//! RESP2/RESP3 codec (ported from `spike/rust/src/resp.rs`) lives in the
//! `gamestore-protocol` crate and lands in **I-02**, at which point the DataNode
//! stops carrying its own parser.

use std::io;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// A RESP2 reply value.
pub enum Reply {
    Simple(String),
    Error(String),
    Bulk(Option<Vec<u8>>),
}

impl Reply {
    /// Serialize this reply to the writer in RESP2 wire format.
    pub async fn write_to<W: AsyncWrite + Unpin>(&self, w: &mut W) -> io::Result<()> {
        match self {
            Reply::Simple(s) => {
                w.write_all(format!("+{s}\r\n").as_bytes()).await?;
            }
            Reply::Error(s) => {
                w.write_all(format!("-{s}\r\n").as_bytes()).await?;
            }
            Reply::Bulk(None) => {
                w.write_all(b"$-1\r\n").await?;
            }
            Reply::Bulk(Some(bytes)) => {
                w.write_all(format!("${}\r\n", bytes.len()).as_bytes())
                    .await?;
                w.write_all(bytes).await?;
                w.write_all(b"\r\n").await?;
            }
        }
        Ok(())
    }
}

/// Read one client command as raw argument byte-vectors.
///
/// Returns `Ok(None)` on a clean EOF. Supports RESP arrays (the normal client
/// encoding) and simple inline commands (handy for manual `nc` testing).
pub async fn read_command<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> io::Result<Option<Vec<Vec<u8>>>> {
    let mut line = Vec::new();
    if read_line(reader, &mut line).await? == 0 {
        return Ok(None);
    }
    if line.is_empty() {
        return Ok(Some(Vec::new()));
    }

    if line[0] == b'*' {
        let count = parse_int(&line[1..])?;
        if count <= 0 {
            return Ok(Some(Vec::new()));
        }
        let mut args = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let mut hdr = Vec::new();
            if read_line(reader, &mut hdr).await? == 0 {
                return Ok(None);
            }
            if hdr.is_empty() || hdr[0] != b'$' {
                return Err(proto_err("expected bulk string"));
            }
            let len = parse_int(&hdr[1..])?;
            if len < 0 {
                args.push(Vec::new());
                continue;
            }
            let mut buf = vec![0u8; len as usize + 2]; // value + CRLF
            reader.read_exact(&mut buf).await?;
            buf.truncate(len as usize);
            args.push(buf);
        }
        Ok(Some(args))
    } else {
        let args = line
            .split(|b| *b == b' ' || *b == b'\t')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_vec())
            .collect();
        Ok(Some(args))
    }
}

/// Read up to and including `\n`, stripping the trailing CRLF/LF.
/// Returns the number of bytes read (0 on EOF).
async fn read_line<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    out: &mut Vec<u8>,
) -> io::Result<usize> {
    let mut raw = Vec::new();
    let n = reader.read_until(b'\n', &mut raw).await?;
    if n == 0 {
        return Ok(0);
    }
    while raw.last() == Some(&b'\n') || raw.last() == Some(&b'\r') {
        raw.pop();
    }
    *out = raw;
    Ok(n)
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
