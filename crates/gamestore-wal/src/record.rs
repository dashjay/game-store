//! WAL record model and on-disk framing.
//!
//! A [`WalRecord`] is the **redo image of one atomic write batch**: the exact
//! set of physical key puts/deletes the engine will apply for a single
//! committed command. Logging the *physical* mutation (rather than the Redis
//! command) is what makes replay **idempotent** — re-applying the same
//! `Put`/`Delete` set reproduces the identical engine state, so recovering the
//! same record twice (crash mid-checkpoint, overlapping segments, …) is
//! harmless. Non-idempotent commands (`INCR`, `LPUSH`, …) would *not* be safe
//! to re-run, but their already-resolved physical effects always are.
//!
//! On-disk framing (little-endian), one frame per record:
//!
//! ```text
//! [payload_len: u32][crc32: u32][payload]
//! ```
//!
//! `crc32` (IEEE, via `crc32fast`) covers `payload` only. On replay a frame
//! whose length runs past the file end, or whose CRC does not match, is treated
//! as a **torn / corrupt tail**: recovery stops there and the file is truncated
//! to the last good frame (see [`crate::file`]). This is the standard WAL
//! contract — a broken frame invalidates everything after it because the
//! framing can no longer be trusted.
//!
//! Payload layout:
//!
//! ```text
//! [lsn: u64][partition: u32][op_count: u32]
//! repeated op_count times:
//!   [tag: u8]                       (0 = Put, 1 = Delete)
//!   [key_len: u32][key bytes]
//!   if Put: [val_len: u32][val bytes]
//! ```

use crate::error::{Result, WalError};

/// Log sequence number: a strictly increasing per-WAL record id (starts at 1).
pub type Lsn = u64;

/// Fixed frame header: `payload_len(4) + crc32(4)`.
pub(crate) const FRAME_HEADER_LEN: usize = 8;

/// A single physical mutation inside a [`WalRecord`].
///
/// Mirrors [`gamestore_engine::WriteOp`] but is a **distinct on-disk type**:
/// the WAL wire format must stay stable independent of the engine's in-memory
/// enum. Conversion happens in [`crate::engine::WalEngine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalOp {
    /// Insert or overwrite `key` with `value`.
    Put(Vec<u8>, Vec<u8>),
    /// Delete `key`.
    Delete(Vec<u8>),
}

/// The redo image of one committed write batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalRecord {
    /// Reserved partition/replica id. Today a DataNode runs a single logical
    /// Core with one store, so this is always `0`; it leaves room for the
    /// Phase-2 "multiple Replicas per Core share one WAL" model without a
    /// format change (plan §1, `docs/design/03-storage-engine.md` §6).
    pub partition: u32,
    /// The physical mutations to apply atomically.
    pub ops: Vec<WalOp>,
}

impl WalRecord {
    /// A record for `ops` in the default (single) partition.
    pub fn new(ops: Vec<WalOp>) -> Self {
        WalRecord { partition: 0, ops }
    }

    /// Serialize the record payload (without the outer frame header) for `lsn`.
    pub(crate) fn encode_payload(&self, lsn: Lsn) -> Vec<u8> {
        let mut buf = Vec::with_capacity(16 + self.ops.len() * 16);
        buf.extend_from_slice(&lsn.to_le_bytes());
        buf.extend_from_slice(&self.partition.to_le_bytes());
        buf.extend_from_slice(&(self.ops.len() as u32).to_le_bytes());
        for op in &self.ops {
            match op {
                WalOp::Put(k, v) => {
                    buf.push(0);
                    put_bytes(&mut buf, k);
                    put_bytes(&mut buf, v);
                }
                WalOp::Delete(k) => {
                    buf.push(1);
                    put_bytes(&mut buf, k);
                }
            }
        }
        buf
    }

    /// Parse a record payload previously produced by [`Self::encode_payload`],
    /// returning `(lsn, record)`.
    pub(crate) fn decode_payload(mut buf: &[u8]) -> Result<(Lsn, WalRecord)> {
        let lsn = take_u64(&mut buf)?;
        let partition = take_u32(&mut buf)?;
        let op_count = take_u32(&mut buf)?;
        let mut ops = Vec::with_capacity(op_count as usize);
        for _ in 0..op_count {
            let tag = take_u8(&mut buf)?;
            match tag {
                0 => {
                    let k = take_bytes(&mut buf)?;
                    let v = take_bytes(&mut buf)?;
                    ops.push(WalOp::Put(k, v));
                }
                1 => {
                    let k = take_bytes(&mut buf)?;
                    ops.push(WalOp::Delete(k));
                }
                other => {
                    return Err(WalError::corruption(format!("unknown wal op tag {other}")));
                }
            }
        }
        if !buf.is_empty() {
            return Err(WalError::corruption("trailing bytes after wal record"));
        }
        Ok((lsn, WalRecord { partition, ops }))
    }

    /// Frame this record for `lsn`: `[payload_len][crc32][payload]`.
    pub(crate) fn encode_frame(&self, lsn: Lsn) -> Vec<u8> {
        let payload = self.encode_payload(lsn);
        let crc = crc32fast::hash(&payload);
        let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&crc.to_le_bytes());
        frame.extend_from_slice(&payload);
        frame
    }
}

fn put_bytes(buf: &mut Vec<u8>, b: &[u8]) {
    buf.extend_from_slice(&(b.len() as u32).to_le_bytes());
    buf.extend_from_slice(b);
}

fn take_u8(buf: &mut &[u8]) -> Result<u8> {
    let (first, rest) = buf.split_first().ok_or_else(|| short("u8"))?;
    *buf = rest;
    Ok(*first)
}

fn take_u32(buf: &mut &[u8]) -> Result<u32> {
    if buf.len() < 4 {
        return Err(short("u32"));
    }
    let (head, rest) = buf.split_at(4);
    *buf = rest;
    Ok(u32::from_le_bytes(head.try_into().unwrap()))
}

fn take_u64(buf: &mut &[u8]) -> Result<u64> {
    if buf.len() < 8 {
        return Err(short("u64"));
    }
    let (head, rest) = buf.split_at(8);
    *buf = rest;
    Ok(u64::from_le_bytes(head.try_into().unwrap()))
}

fn take_bytes(buf: &mut &[u8]) -> Result<Vec<u8>> {
    let len = take_u32(buf)? as usize;
    if buf.len() < len {
        return Err(short("length-prefixed bytes"));
    }
    let (head, rest) = buf.split_at(len);
    *buf = rest;
    Ok(head.to_vec())
}

fn short(what: &str) -> WalError {
    WalError::corruption(format!("payload too short reading {what}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> WalRecord {
        WalRecord::new(vec![
            WalOp::Put(b"meta:player".to_vec(), b"value".to_vec()),
            WalOp::Delete(b"old".to_vec()),
            WalOp::Put(Vec::new(), Vec::new()),
        ])
    }

    #[test]
    fn payload_roundtrips() {
        let rec = sample();
        let payload = rec.encode_payload(42);
        let (lsn, decoded) = WalRecord::decode_payload(&payload).unwrap();
        assert_eq!(lsn, 42);
        assert_eq!(decoded, rec);
    }

    #[test]
    fn frame_layout_is_len_crc_payload() {
        let rec = sample();
        let frame = rec.encode_frame(7);
        let len = u32::from_le_bytes(frame[0..4].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(frame[4..8].try_into().unwrap());
        let payload = &frame[FRAME_HEADER_LEN..];
        assert_eq!(len, payload.len());
        assert_eq!(crc, crc32fast::hash(payload));
        let (lsn, decoded) = WalRecord::decode_payload(payload).unwrap();
        assert_eq!(lsn, 7);
        assert_eq!(decoded, rec);
    }

    #[test]
    fn decode_rejects_short_and_trailing() {
        assert!(WalRecord::decode_payload(&[0u8; 3]).is_err());
        let mut p = sample().encode_payload(1);
        p.push(0xff); // trailing garbage
        assert!(WalRecord::decode_payload(&p).is_err());
    }

    #[test]
    fn decode_rejects_unknown_op_tag() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&1u64.to_le_bytes()); // lsn
        buf.extend_from_slice(&0u32.to_le_bytes()); // partition
        buf.extend_from_slice(&1u32.to_le_bytes()); // op_count
        buf.push(9); // bad tag
        assert!(WalRecord::decode_payload(&buf).is_err());
    }
}
