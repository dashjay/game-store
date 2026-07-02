//! Segmented file [`Wal`] with group-commit `fsync` and crash recovery.
//!
//! # Layout
//!
//! The log is a directory of **segment** files named `{start_lsn:020}.wal`,
//! each a sequence of frames (`[len][crc32][payload]`, see [`crate::record`]).
//! Appends go to the newest ("active") segment; when it exceeds
//! `segment_max_bytes` the next append rolls to a fresh segment. Only whole
//! segments below a checkpoint are removed by [`Wal::truncate`], so GC never
//! rewrites live data.
//!
//! # Durability & group commit
//!
//! [`FileWal::append`] only `write(2)`s the frame (into the OS page cache) and
//! assigns an [`Lsn`]. [`FileWal::sync`] is what makes records durable, and it
//! is designed so that **concurrent writers share one `fsync`**: the first
//! caller becomes the *leader*, snapshots how far the log has been written,
//! releases the lock and issues a single `fsync`; every other caller that
//! arrives while that `fsync` is in flight simply waits and is covered by it
//! (leader/follower group commit). Under N concurrent writers this collapses N
//! `fsync`s toward one — the "fewer fsyncs" evidence the I-08 DoD asks for.
//!
//! # Invariant that makes a single active-file `fsync` sufficient
//!
//! Every **non-active** segment is fully `fsync`'d at the moment it is rolled
//! off, so the only records that can be un-synced live in the active segment.
//! A leader therefore only needs to `fsync` the active file (captured under the
//! lock) to persist everything appended up to its snapshot LSN.
//!
//! # Recovery
//!
//! [`FileWal::open`] scans segments in LSN order and validates every frame. The
//! first frame with an incomplete header/payload or a CRC mismatch is treated
//! as a **torn / corrupt tail**: the file is truncated to the last good frame,
//! all later segments are deleted, and scanning stops. This repairs both a
//! crash mid-write (torn tail) and bit-rot (bad CRC) into a clean prefix that
//! [`Wal::replay`] can hand back for idempotent re-application.

use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use crate::error::{Result, WalError};
use crate::record::{Lsn, WalRecord, FRAME_HEADER_LEN};
use crate::wal::{Replayed, Wal};

/// Default active-segment size before rolling to a new segment (64 MiB).
pub const DEFAULT_SEGMENT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Tuning parameters for [`FileWal`].
#[derive(Debug, Clone)]
pub struct WalConfig {
    /// Roll to a new segment once the active one reaches this size.
    pub segment_max_bytes: u64,
}

impl Default for WalConfig {
    fn default() -> Self {
        WalConfig {
            segment_max_bytes: DEFAULT_SEGMENT_MAX_BYTES,
        }
    }
}

/// One on-disk segment. The segment's starting LSN lives in its file name
/// (`{start_lsn:020}.wal`); only the fields consulted at runtime are kept here.
struct Segment {
    /// LSN of the last record actually written here (`0` when empty).
    end_lsn: Lsn,
    /// Bytes currently in the segment file.
    bytes: u64,
    path: PathBuf,
}

struct State {
    segments: Vec<Segment>,
    /// Handle to the active (last) segment, positioned at end for appends.
    active: File,
    /// LSN to assign to the next appended record.
    next_lsn: Lsn,
    /// Last appended (written-to-page-cache) LSN; `0` when empty.
    write_lsn: Lsn,
    /// Last `fsync`'d LSN; `0` when nothing durable yet.
    durable_lsn: Lsn,
    /// Total retained bytes across all segments (`wal_gc_pending`).
    total_bytes: u64,
    /// Whether a leader is currently mid-`fsync` (group-commit coordination).
    syncing: bool,
}

/// Segmented, group-committing, crash-recoverable file WAL.
pub struct FileWal {
    dir: PathBuf,
    segment_max_bytes: u64,
    state: Mutex<State>,
    sync_cv: Condvar,
    fsync_count: AtomicU64,
}

impl FileWal {
    /// Open (creating if needed) a WAL in `dir`, repairing any torn/corrupt
    /// tail so the log is a clean prefix ready for append and replay.
    pub fn open(dir: impl AsRef<Path>, config: &WalConfig) -> Result<FileWal> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;

        let mut files = discover_segments(&dir)?;
        files.sort_by_key(|(lsn, _)| *lsn);

        let mut segments: Vec<Segment> = Vec::new();
        let mut last_good_lsn: Lsn = 0;
        let mut total_bytes: u64 = 0;
        let mut truncated = false;

        for (idx, (_start_lsn, path)) in files.iter().enumerate() {
            let scan = scan_segment(path)?;
            let seg = Segment {
                end_lsn: scan.last_lsn,
                bytes: scan.good_bytes,
                path: path.clone(),
            };
            if scan.last_lsn != 0 {
                last_good_lsn = scan.last_lsn;
            }
            total_bytes += scan.good_bytes;
            segments.push(seg);

            if scan.torn {
                // Repair: truncate this segment to its last good frame and drop
                // every later segment — a broken frame invalidates all that
                // follows because the framing can no longer be trusted.
                truncate_file(path, scan.good_bytes)?;
                for (_, later) in files.iter().skip(idx + 1) {
                    fs::remove_file(later)?;
                }
                truncated = true;
                break;
            }
        }
        if truncated {
            fsync_dir(&dir)?;
        }

        // Establish (or create) the active segment.
        let (segments, active, next_lsn) = if segments.is_empty() {
            let start_lsn = 1;
            let path = segment_path(&dir, start_lsn);
            let active = open_segment(&path)?;
            fsync_dir(&dir)?;
            (
                vec![Segment {
                    end_lsn: 0,
                    bytes: 0,
                    path,
                }],
                active,
                start_lsn,
            )
        } else {
            let active_path = segments.last().unwrap().path.clone();
            let mut active = open_segment(&active_path)?;
            active.seek(SeekFrom::End(0))?;
            let next_lsn = last_good_lsn + 1;
            (segments, active, next_lsn)
        };

        let state = State {
            segments,
            active,
            next_lsn,
            write_lsn: last_good_lsn,
            durable_lsn: last_good_lsn,
            total_bytes,
            syncing: false,
        };

        Ok(FileWal {
            dir,
            segment_max_bytes: config.segment_max_bytes.max(1),
            state: Mutex::new(state),
            sync_cv: Condvar::new(),
            fsync_count: AtomicU64::new(0),
        })
    }

    /// Record one `fsync` and its latency for `wal_fsync_latency_seconds`.
    fn observe_fsync(&self, file: &File) -> Result<()> {
        let start = Instant::now();
        file.sync_data()?;
        metrics::histogram!("wal_fsync_latency_seconds").record(start.elapsed().as_secs_f64());
        self.fsync_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl Wal for FileWal {
    fn append(&self, records: &[WalRecord]) -> Result<Lsn> {
        let mut st = self.state.lock().unwrap();
        let mut last_lsn = st.write_lsn;
        for record in records {
            let lsn = st.next_lsn;
            let frame = record.encode_frame(lsn);
            st.active.write_all(&frame)?;
            let frame_len = frame.len() as u64;
            st.next_lsn += 1;
            st.write_lsn = lsn;
            st.total_bytes += frame_len;
            last_lsn = lsn;
            {
                let seg = st.segments.last_mut().unwrap();
                seg.end_lsn = lsn;
                seg.bytes += frame_len;
            }
            if st.segments.last().unwrap().bytes >= self.segment_max_bytes {
                self.roll(&mut st)?;
            }
        }
        Ok(last_lsn)
    }

    fn sync(&self) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        loop {
            let target = st.write_lsn;
            if st.durable_lsn >= target {
                return Ok(());
            }
            if st.syncing {
                st = self.sync_cv.wait(st).unwrap();
                continue;
            }
            // Become the group-commit leader for this round.
            st.syncing = true;
            let snapshot = st.write_lsn;
            let file = st.active.try_clone()?;
            drop(st);

            let result = self.observe_fsync(&file);

            st = self.state.lock().unwrap();
            st.syncing = false;
            if result.is_ok() {
                st.durable_lsn = st.durable_lsn.max(snapshot);
            }
            self.sync_cv.notify_all();
            result?;
            // Loop re-checks: our (possibly newer) target may need another round.
        }
    }

    fn replay(&self, from: Lsn) -> Result<Vec<Replayed>> {
        let paths: Vec<PathBuf> = {
            let st = self.state.lock().unwrap();
            st.segments.iter().map(|s| s.path.clone()).collect()
        };
        let mut out = Vec::new();
        for path in paths {
            let frames = read_good_frames(&path)?;
            for (lsn, record) in frames {
                if lsn >= from {
                    out.push(Replayed { lsn, record });
                }
            }
        }
        Ok(out)
    }

    fn truncate(&self, upto: Lsn) -> Result<()> {
        let mut st = self.state.lock().unwrap();
        let mut removed_any = false;
        while st.segments.len() > 1 {
            let front = &st.segments[0];
            // Keep any segment that still holds a record at or above `upto`.
            if front.end_lsn == 0 || front.end_lsn >= upto {
                break;
            }
            let seg = st.segments.remove(0);
            st.total_bytes = st.total_bytes.saturating_sub(seg.bytes);
            fs::remove_file(&seg.path)?;
            removed_any = true;
        }
        if removed_any {
            fsync_dir(&self.dir)?;
        }
        Ok(())
    }

    fn next_lsn(&self) -> Lsn {
        self.state.lock().unwrap().next_lsn
    }

    fn pending_bytes(&self) -> u64 {
        self.state.lock().unwrap().total_bytes
    }

    fn fsync_count(&self) -> u64 {
        self.fsync_count.load(Ordering::Relaxed)
    }
}

impl FileWal {
    /// Roll the active segment: `fsync` it (upholding the "non-active segments
    /// are durable" invariant), then create and switch to a fresh segment.
    fn roll(&self, st: &mut State) -> Result<()> {
        self.observe_fsync(&st.active)?;
        st.durable_lsn = st.durable_lsn.max(st.write_lsn);

        let start_lsn = st.next_lsn;
        let path = segment_path(&self.dir, start_lsn);
        let active = open_segment(&path)?;
        fsync_dir(&self.dir)?;
        st.segments.push(Segment {
            end_lsn: 0,
            bytes: 0,
            path,
        });
        st.active = active;
        Ok(())
    }
}

/// Result of scanning one segment file for valid frames.
struct SegmentScan {
    /// Bytes covered by well-formed, CRC-valid frames.
    good_bytes: u64,
    /// LSN of the last good record (`0` when the segment held none).
    last_lsn: Lsn,
    /// Whether a torn/corrupt frame was found past `good_bytes`.
    torn: bool,
}

/// Scan a segment, stopping at the first malformed/CRC-bad frame.
fn scan_segment(path: &Path) -> Result<SegmentScan> {
    let raw = fs::read(path)?;
    let mut offset = 0usize;
    let mut last_lsn = 0;
    let mut torn = false;
    while offset < raw.len() {
        match decode_frame_at(&raw, offset) {
            Some((lsn, _record, next)) => {
                last_lsn = lsn;
                offset = next;
            }
            None => {
                torn = true;
                break;
            }
        }
    }
    Ok(SegmentScan {
        good_bytes: offset as u64,
        last_lsn,
        torn,
    })
}

/// Read every well-formed frame from a segment (stopping at the first bad one).
fn read_good_frames(path: &Path) -> Result<Vec<(Lsn, WalRecord)>> {
    let raw = fs::read(path)?;
    let mut offset = 0usize;
    let mut out = Vec::new();
    while offset < raw.len() {
        match decode_frame_at(&raw, offset) {
            Some((lsn, record, next)) => {
                out.push((lsn, record));
                offset = next;
            }
            None => break,
        }
    }
    Ok(out)
}

/// Decode the frame at `offset`, returning `(lsn, record, next_offset)` or
/// `None` if the header/payload is incomplete or the CRC does not match.
fn decode_frame_at(raw: &[u8], offset: usize) -> Option<(Lsn, WalRecord, usize)> {
    if offset + FRAME_HEADER_LEN > raw.len() {
        return None;
    }
    let len = u32::from_le_bytes(raw[offset..offset + 4].try_into().ok()?) as usize;
    let crc = u32::from_le_bytes(raw[offset + 4..offset + 8].try_into().ok()?);
    let payload_start = offset + FRAME_HEADER_LEN;
    let payload_end = payload_start.checked_add(len)?;
    if payload_end > raw.len() {
        return None;
    }
    let payload = &raw[payload_start..payload_end];
    if crc32fast::hash(payload) != crc {
        return None;
    }
    let (lsn, record) = WalRecord::decode_payload(payload).ok()?;
    Some((lsn, record, payload_end))
}

/// List `*.wal` files in `dir` as `(start_lsn, path)`.
fn discover_segments(dir: &Path) -> Result<Vec<(Lsn, PathBuf)>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wal") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or_else(|| WalError::corruption("non-utf8 wal segment name"))?;
        let lsn: Lsn = stem
            .parse()
            .map_err(|_| WalError::corruption(format!("unparseable wal segment name '{stem}'")))?;
        out.push((lsn, path));
    }
    Ok(out)
}

fn segment_path(dir: &Path, start_lsn: Lsn) -> PathBuf {
    dir.join(format!("{start_lsn:020}.wal"))
}

fn open_segment(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?)
}

fn truncate_file(path: &Path, len: u64) -> Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(len)?;
    file.sync_all()?;
    Ok(())
}

/// `fsync` the directory so segment create/delete entries survive power loss.
fn fsync_dir(dir: &Path) -> Result<()> {
    let handle = File::open(dir)?;
    // Directory fsync is best-effort on some platforms; ignore ENOTSUP-style
    // failures rather than failing an otherwise-successful append.
    match handle.sync_all() {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc_einval()) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// `EINVAL` — some filesystems reject `fsync` on a directory handle.
fn libc_einval() -> i32 {
    22
}
