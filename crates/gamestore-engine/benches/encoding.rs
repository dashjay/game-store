//! Micro-benchmarks for the on-disk encoding (I-07): metadata encode/decode,
//! subkey build/parse and the ZSet order-preserving score transform.
//!
//! These sit on every read/write path, so they establish the per-record
//! encoding overhead baseline (expected: tens of nanoseconds, allocation
//! dominated). Run with `cargo bench -p gamestore-engine --bench encoding`.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use gamestore_engine::encoding::{
    decode_score, encode_score, meta_key, parse_subkey, subkey, zscore_key, Meta, TYPE_HASH,
};

fn bench_encoding(c: &mut Criterion) {
    let key: &[u8] = b"player:{1001}";
    let field: &[u8] = b"gold";

    c.bench_function("meta_key", |b| b.iter(|| meta_key(black_box(key))));

    let mut meta = Meta {
        type_id: TYPE_HASH,
        version: 1_720_000_000_000_000,
        expire_ms: 0,
        payload: Vec::new(),
    };
    meta.set_field_count(50);
    c.bench_function("meta_encode", |b| b.iter(|| black_box(&meta).encode()));

    let raw_meta = meta.encode();
    c.bench_function("meta_decode", |b| b.iter(|| Meta::decode(black_box(&raw_meta))));

    c.bench_function("subkey_build", |b| {
        b.iter(|| subkey(black_box(key), black_box(meta.version), black_box(field)))
    });

    let raw_subkey = subkey(key, meta.version, field);
    c.bench_function("subkey_parse", |b| {
        b.iter(|| parse_subkey(black_box(&raw_subkey)))
    });

    c.bench_function("zscore_key_build", |b| {
        b.iter(|| {
            zscore_key(
                black_box(key),
                black_box(meta.version),
                black_box(42.5),
                black_box(field),
            )
        })
    });

    c.bench_function("score_encode_decode", |b| {
        b.iter(|| decode_score(encode_score(black_box(-1234.5678))))
    });
}

criterion_group!(benches, bench_encoding);
criterion_main!(benches);
