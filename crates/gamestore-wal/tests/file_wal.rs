//! Integration tests for [`FileWal`] (I-08 DoD): crash recovery without loss,
//! idempotent-friendly replay, CRC-corrupt / torn-tail truncation, group-commit
//! fsync coalescing, and segment GC via `truncate`.

use std::fs::{self, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::sync::{Arc, Barrier};
use std::thread;

use gamestore_wal::{FileWal, Wal, WalConfig, WalOp, WalRecord};
use tempfile::TempDir;

fn rec(key: &str, val: &str) -> WalRecord {
    WalRecord::new(vec![WalOp::Put(
        key.as_bytes().to_vec(),
        val.as_bytes().to_vec(),
    )])
}

fn open(dir: &TempDir) -> FileWal {
    FileWal::open(dir.path(), &WalConfig::default()).expect("open wal")
}

#[test]
fn append_sync_replay_roundtrip() {
    let dir = TempDir::new().unwrap();
    let wal = open(&dir);
    let l1 = wal.append(&[rec("a", "1")]).unwrap();
    let l2 = wal.append(&[rec("b", "2")]).unwrap();
    assert_eq!((l1, l2), (1, 2));
    wal.sync().unwrap();

    let replayed = wal.replay(1).unwrap();
    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0].lsn, 1);
    assert_eq!(replayed[0].record, rec("a", "1"));
    assert_eq!(replayed[1].record, rec("b", "2"));
}

#[test]
fn replay_from_offset_skips_earlier_records() {
    let dir = TempDir::new().unwrap();
    let wal = open(&dir);
    wal.append(&[rec("a", "1")]).unwrap();
    wal.append(&[rec("b", "2")]).unwrap();
    wal.append(&[rec("c", "3")]).unwrap();
    wal.sync().unwrap();

    let from2 = wal.replay(2).unwrap();
    assert_eq!(from2.len(), 2);
    assert_eq!(from2[0].lsn, 2);
    assert_eq!(from2[1].lsn, 3);
}

/// Simulate a crash (kill -9): records synced to disk are recovered on reopen,
/// LSNs continue monotonically, and nothing is lost or duplicated.
#[test]
fn crash_recovery_reopen_replays_all_confirmed_writes() {
    let dir = TempDir::new().unwrap();
    {
        let wal = open(&dir);
        wal.append(&[rec("k1", "v1")]).unwrap();
        wal.append(&[rec("k2", "v2")]).unwrap();
        wal.sync().unwrap();
        // Drop without truncating — the process "crashes" here.
    }
    let wal = open(&dir);
    let replayed = wal.replay(1).unwrap();
    assert_eq!(replayed.len(), 2);
    assert_eq!(replayed[0].record, rec("k1", "v1"));
    assert_eq!(replayed[1].record, rec("k2", "v2"));

    // New appends continue after the recovered tail without LSN collision.
    assert_eq!(wal.next_lsn(), 3);
    let l3 = wal.append(&[rec("k3", "v3")]).unwrap();
    assert_eq!(l3, 3);
    wal.sync().unwrap();
    assert_eq!(wal.replay(1).unwrap().len(), 3);
}

/// A torn tail (partial frame from an interrupted write) is truncated off on
/// recovery, leaving a clean prefix; later good records are unaffected.
#[test]
fn torn_tail_is_truncated_on_recovery() {
    let dir = TempDir::new().unwrap();
    let seg_path;
    {
        let wal = open(&dir);
        wal.append(&[rec("good1", "v")]).unwrap();
        wal.append(&[rec("good2", "v")]).unwrap();
        wal.sync().unwrap();
        seg_path = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().and_then(|x| x.to_str()) == Some("wal"))
            .unwrap();
    }
    // Append a partial frame header (fewer bytes than the declared payload):
    // exactly what an interrupted write leaves behind.
    {
        let mut f = OpenOptions::new().append(true).open(&seg_path).unwrap();
        f.write_all(&999u32.to_le_bytes()).unwrap(); // claims 999-byte payload
        f.write_all(&0u32.to_le_bytes()).unwrap(); // crc
        f.write_all(b"only a few bytes").unwrap(); // ...but far fewer follow
        f.flush().unwrap();
    }
    let len_before = fs::metadata(&seg_path).unwrap().len();

    let wal = open(&dir);
    let replayed = wal.replay(1).unwrap();
    assert_eq!(replayed.len(), 2, "only the two good records survive");
    assert_eq!(replayed[1].record, rec("good2", "v"));

    // The torn bytes were physically removed.
    let len_after = fs::metadata(&seg_path).unwrap().len();
    assert!(len_after < len_before, "torn tail should be truncated away");

    // Recovery is clean enough to keep appending.
    assert_eq!(wal.next_lsn(), 3);
    wal.append(&[rec("good3", "v")]).unwrap();
    wal.sync().unwrap();
    assert_eq!(wal.replay(1).unwrap().len(), 3);
}

/// A CRC-corrupt record (bit-rot) stops recovery at that record: the framing
/// can no longer be trusted, so it and everything after are discarded.
#[test]
fn crc_corruption_stops_recovery_at_the_bad_record() {
    let dir = TempDir::new().unwrap();
    let seg_path;
    {
        let wal = open(&dir);
        wal.append(&[rec("a", "1")]).unwrap();
        wal.append(&[rec("b", "2")]).unwrap();
        wal.append(&[rec("c", "3")]).unwrap();
        wal.sync().unwrap();
        seg_path = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().and_then(|x| x.to_str()) == Some("wal"))
            .unwrap();
    }
    // Corrupt a byte inside the SECOND record's payload. Frame 1:
    // [len:4][crc:4][payload]. Flip a payload byte of frame 2.
    {
        let bytes = fs::read(&seg_path).unwrap();
        let len1 = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        let frame2_payload_start = 8 + len1 + 8; // header1 + payload1 + header2
        let mut f = OpenOptions::new().write(true).open(&seg_path).unwrap();
        f.seek(SeekFrom::Start(frame2_payload_start as u64))
            .unwrap();
        let orig = bytes[frame2_payload_start];
        f.write_all(&[orig ^ 0xff]).unwrap();
        f.flush().unwrap();
    }

    let wal = open(&dir);
    let replayed = wal.replay(1).unwrap();
    assert_eq!(replayed.len(), 1, "recovery stops at the corrupt record");
    assert_eq!(replayed[0].record, rec("a", "1"));
}

/// Concurrent writers that each append + sync share `fsync`s: the syscall count
/// is far below the number of writers (group commit — the I-08 DoD evidence).
#[test]
fn group_commit_coalesces_concurrent_fsyncs() {
    let dir = TempDir::new().unwrap();
    let wal = Arc::new(open(&dir));
    const WRITERS: usize = 16;
    let barrier = Arc::new(Barrier::new(WRITERS));

    let mut handles = Vec::new();
    for i in 0..WRITERS {
        let wal = wal.clone();
        let barrier = barrier.clone();
        handles.push(thread::spawn(move || {
            wal.append(&[rec(&format!("k{i}"), "v")]).unwrap();
            // All writers reach the fsync point together, so one leader's fsync
            // can cover the whole batch.
            barrier.wait();
            wal.sync().unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let fsyncs = wal.fsync_count();
    assert!(
        fsyncs < WRITERS as u64,
        "expected group commit to coalesce {WRITERS} syncs, got {fsyncs} fsyncs"
    );
    assert!(fsyncs >= 1, "at least one fsync must have happened");
    assert_eq!(wal.replay(1).unwrap().len(), WRITERS);
}

/// `truncate` GCs whole segments below the checkpoint while keeping the records
/// at/after it replayable; `pending_bytes` shrinks accordingly.
#[test]
fn truncate_gcs_old_segments() {
    let dir = TempDir::new().unwrap();
    // Tiny segments so a handful of records roll several files.
    let wal = FileWal::open(
        dir.path(),
        &WalConfig {
            segment_max_bytes: 64,
        },
    )
    .unwrap();
    for i in 0..20 {
        wal.append(&[rec(&format!("k{i:02}"), "value-payload")])
            .unwrap();
    }
    wal.sync().unwrap();
    let before = wal.pending_bytes();
    let segments_before = wal_segment_count(dir.path());
    assert!(segments_before > 1, "expected multiple segments");

    // Checkpoint says records < 15 are durable in the engine.
    wal.truncate(15).unwrap();

    let after = wal.pending_bytes();
    assert!(after < before, "pending bytes should drop after truncate");
    assert!(wal_segment_count(dir.path()) < segments_before);

    // Records at/after the checkpoint are still fully replayable.
    let replayed = wal.replay(15).unwrap();
    assert_eq!(replayed.first().map(|r| r.lsn), Some(15));
    assert_eq!(replayed.last().map(|r| r.lsn), Some(20));
}

fn wal_segment_count(dir: &std::path::Path) -> usize {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("wal"))
        .count()
}
