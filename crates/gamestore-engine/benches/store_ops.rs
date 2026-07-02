//! Micro-benchmarks for single `Store` operations on a real RocksDB engine
//! (I-07): the engine-layer cost of one command's work, without protocol or
//! dispatch overhead.
//!
//! Together with `gamestore-datamodel/benches/commands.rs` (same operations
//! through the command registry) this isolates the command-layer overhead and
//! provides the numbers for re-evaluating MR-0018's "run engine calls inline
//! on the runtime" decision. Run with
//! `cargo bench -p gamestore-engine --bench store_ops`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use gamestore_engine::{EngineConfig, RocksEngine, Store};
use tempfile::TempDir;

fn fresh_store() -> (Store<RocksEngine>, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let store = Store::open(dir.path(), &EngineConfig::default()).expect("open store");
    (store, dir)
}

fn bench_store_ops(c: &mut Criterion) {
    let (store, _dir) = fresh_store();

    // Typical player-state payloads: small keys, small values (01-workload).
    let value = vec![0x42u8; 64];

    c.bench_function("store_set", |b| {
        let mut i = 0u64;
        b.iter(|| {
            // Rotate over a bounded keyspace so the memtable stays realistic.
            let key = format!("bench:str:{}", i % 10_000);
            i += 1;
            store.set(black_box(key.as_bytes()), black_box(&value), 0).unwrap()
        })
    });

    store.set(b"bench:get", &value, 0).unwrap();
    c.bench_function("store_get", |b| {
        b.iter(|| store.get(black_box(b"bench:get")).unwrap())
    });

    c.bench_function("store_hset_field", |b| {
        let mut i = 0u64;
        b.iter(|| {
            let field = format!("f{}", i % 50);
            i += 1;
            store
                .hset(
                    black_box(b"bench:hash"),
                    &[(field.into_bytes(), value.clone())],
                )
                .unwrap()
        })
    });

    c.bench_function("store_hget", |b| {
        b.iter(|| store.hget(black_box(b"bench:hash"), black_box(b"f7")).unwrap())
    });

    c.bench_function("store_zadd_update", |b| {
        let mut i = 0u64;
        b.iter(|| {
            let score = (i % 1000) as f64;
            i += 1;
            store
                .zadd(black_box(b"bench:zset"), &[(score, b"member".to_vec())])
                .unwrap()
        })
    });

    c.bench_function("store_lpush_rpop", |b| {
        b.iter(|| {
            store.push(black_box(b"bench:list"), &[value.clone()], true).unwrap();
            store.pop(black_box(b"bench:list"), 1, false).unwrap()
        })
    });
}

criterion_group!(benches, bench_store_ops);
criterion_main!(benches);
