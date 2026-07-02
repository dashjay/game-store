//! Micro-benchmark quantifying the group-commit payoff (I-08 DoD evidence): the
//! per-record cost of `append + fsync` when every record is fsync'd
//! individually vs. when a whole batch is appended and fsync'd once.
//!
//! `fsync` is the dominant cost of a durable write, so amortizing one `fsync`
//! over `BATCH` records — exactly what group commit does when concurrent
//! writers coalesce — is the win. Compare the two `elements`-normalized
//! throughputs (`cargo bench -p gamestore-wal`): the grouped case reports far
//! more records/second because it issues `BATCH`× fewer `fsync`s.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use gamestore_wal::{FileWal, Wal, WalConfig, WalOp, WalRecord};
use tempfile::TempDir;

const BATCH: usize = 64;

fn record(i: usize) -> WalRecord {
    // A player-field-sized write: a small key and a 64-byte value.
    WalRecord::new(vec![WalOp::Put(
        format!("player:{i}").into_bytes(),
        vec![0u8; 64],
    )])
}

fn bench_group_commit(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_write_path");
    group.throughput(Throughput::Elements(BATCH as u64));

    // One fsync per record: BATCH append+sync pairs.
    group.bench_function("fsync_per_record", |b| {
        let dir = TempDir::new().unwrap();
        let wal = FileWal::open(dir.path(), &WalConfig::default()).unwrap();
        let mut i = 0usize;
        b.iter(|| {
            for _ in 0..BATCH {
                wal.append(std::slice::from_ref(&record(i))).unwrap();
                wal.sync().unwrap();
                i += 1;
            }
        });
    });

    // Group commit: append the whole batch, then a single fsync covers it.
    group.bench_function("grouped_single_fsync", |b| {
        let dir = TempDir::new().unwrap();
        let wal = FileWal::open(dir.path(), &WalConfig::default()).unwrap();
        let mut i = 0usize;
        b.iter(|| {
            for _ in 0..BATCH {
                wal.append(std::slice::from_ref(&record(i))).unwrap();
                i += 1;
            }
            wal.sync().unwrap();
        });
    });

    group.finish();
}

criterion_group!(benches, bench_group_commit);
criterion_main!(benches);
