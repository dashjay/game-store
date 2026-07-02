//! Micro-benchmarks for single commands through the [`CommandRegistry`]
//! (I-07): parsing/arity/dispatch + engine work on a real RocksDB store —
//! i.e. everything a command costs on a DataNode worker except the RESP
//! codec and the socket.
//!
//! Comparing these numbers with `gamestore-engine/benches/store_ops.rs`
//! yields the command-layer overhead; their absolute magnitude (microseconds)
//! is the evidence base for MR-0018's "run engine calls synchronously inline"
//! decision, re-reviewed in I-07. Run with
//! `cargo bench -p gamestore-datamodel --bench commands`.

use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use gamestore_datamodel::{CommandRegistry, ExecCtx};
use gamestore_engine::{EngineConfig, RocksEngine, Store};
use gamestore_protocol::{Frame, RespVersion};
use tempfile::TempDir;

struct Bench {
    store: Store<RocksEngine>,
    registry: CommandRegistry<RocksEngine>,
    _dir: TempDir,
}

impl Bench {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let store = Store::open(dir.path(), &EngineConfig::default()).expect("open store");
        Bench {
            store,
            registry: CommandRegistry::standard(),
            _dir: dir,
        }
    }

    fn exec(&self, args: &[&[u8]]) -> Frame {
        let args: Vec<Bytes> = args.iter().map(|a| Bytes::copy_from_slice(a)).collect();
        let mut ctx = ExecCtx::new(&self.store, RespVersion::V2);
        self.registry.dispatch(&mut ctx, &args)
    }
}

fn bench_commands(c: &mut Criterion) {
    let b = Bench::new();
    let value = vec![0x42u8; 64];

    c.bench_function("cmd_ping", |bch| {
        bch.iter(|| black_box(b.exec(&[b"PING"])))
    });

    c.bench_function("cmd_set", |bch| {
        let mut i = 0u64;
        bch.iter(|| {
            let key = format!("bench:str:{}", i % 10_000);
            i += 1;
            black_box(b.exec(&[b"SET", key.as_bytes(), &value]))
        })
    });

    b.exec(&[b"SET", b"bench:get", &value]);
    c.bench_function("cmd_get", |bch| {
        bch.iter(|| black_box(b.exec(&[b"GET", b"bench:get"])))
    });

    c.bench_function("cmd_hset_field", |bch| {
        let mut i = 0u64;
        bch.iter(|| {
            let field = format!("f{}", i % 50);
            i += 1;
            black_box(b.exec(&[b"HSET", b"bench:hash", field.as_bytes(), &value]))
        })
    });

    c.bench_function("cmd_hget", |bch| {
        bch.iter(|| black_box(b.exec(&[b"HGET", b"bench:hash", b"f7"])))
    });

    c.bench_function("cmd_hgetall_50_fields", |bch| {
        bch.iter(|| black_box(b.exec(&[b"HGETALL", b"bench:hash"])))
    });

    c.bench_function("cmd_zadd_update", |bch| {
        let mut i = 0u64;
        bch.iter(|| {
            let score = format!("{}", i % 1000);
            i += 1;
            black_box(b.exec(&[b"ZADD", b"bench:zset", score.as_bytes(), b"member"]))
        })
    });

    b.exec(&[b"DEL", b"bench:zrange"]);
    for i in 0..100 {
        let score = format!("{i}");
        let member = format!("m{i}");
        b.exec(&[b"ZADD", b"bench:zrange", score.as_bytes(), member.as_bytes()]);
    }
    c.bench_function("cmd_zrange_100", |bch| {
        bch.iter(|| black_box(b.exec(&[b"ZRANGE", b"bench:zrange", b"0", b"-1"])))
    });

    c.bench_function("cmd_lpush_rpop", |bch| {
        bch.iter(|| {
            b.exec(&[b"LPUSH", b"bench:list", &value]);
            black_box(b.exec(&[b"RPOP", b"bench:list"]))
        })
    });
}

criterion_group!(benches, bench_commands);
criterion_main!(benches);
