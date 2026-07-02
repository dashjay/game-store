//! Hash commands: `HSET`, `HMSET`, `HGET`, `HMGET`, `HGETALL`, `HDEL`, `HLEN`,
//! `HEXISTS`.
//!
//! Hash is the primary carrier for player data
//! ([`docs/design/01-workload-data-model.md`]); on disk each field is a subkey
//! under the owner's current structure version
//! ([`docs/design/03-storage-engine.md`] §2.2).

use bytes::Bytes;
use gamestore_engine::GeneralEngine;
use gamestore_protocol::{Frame, RespVersion};

use crate::registry::{bulk, engine_error, wrong_args, CommandRegistry, ExecCtx};

/// Register the Hash commands.
pub fn register<E: GeneralEngine + 'static>(reg: &mut CommandRegistry<E>) {
    reg.register("HSET", -4, hset::<E>);
    reg.register("HMSET", -4, hset::<E>);
    reg.register("HGET", 3, hget::<E>);
    reg.register("HMGET", -3, hmget::<E>);
    reg.register("HGETALL", 2, hgetall::<E>);
    reg.register("HDEL", -3, hdel::<E>);
    reg.register("HLEN", 2, hlen::<E>);
    reg.register("HEXISTS", 3, hexists::<E>);
}

/// `HSET key field value [field value ...]` (also serves `HMSET`).
///
/// `HSET` replies with the number of *newly created* fields; the deprecated
/// `HMSET` alias replies `+OK`. An odd field/value tail is an arity error
/// reported under the invoked name, matching Redis.
fn hset<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let is_hmset = args[0].eq_ignore_ascii_case(b"HMSET");
    if (args.len() - 2) % 2 != 0 {
        return wrong_args(if is_hmset { "hmset" } else { "hset" });
    }
    let pairs: Vec<(Vec<u8>, Vec<u8>)> = args[2..]
        .chunks_exact(2)
        .map(|fv| (fv[0].to_vec(), fv[1].to_vec()))
        .collect();
    match ctx.store.hset(&args[1], &pairs) {
        Ok(_) if is_hmset => Frame::ok(),
        Ok(created) => Frame::Integer(created),
        Err(e) => engine_error(e),
    }
}

/// `HGET key field` — nil when the key or field is missing.
fn hget<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.hget(&args[1], &args[2]) {
        Ok(Some(v)) => bulk(v),
        Ok(None) => Frame::Null,
        Err(e) => engine_error(e),
    }
}

/// `HMGET key field [field ...]` — one bulk-or-nil per requested field.
fn hmget<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let mut items = Vec::with_capacity(args.len() - 2);
    for field in &args[2..] {
        match ctx.store.hget(&args[1], field) {
            Ok(Some(v)) => items.push(bulk(v)),
            Ok(None) => items.push(Frame::Null),
            Err(e) => return engine_error(e),
        }
    }
    Frame::Array(items)
}

/// `HGETALL key` — RESP3 connections get a native map, RESP2 the classic flat
/// field/value array. An empty reply for a missing key, like Redis.
fn hgetall<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let pairs = match ctx.store.hgetall(&args[1]) {
        Ok(pairs) => pairs,
        Err(e) => return engine_error(e),
    };
    match ctx.version {
        RespVersion::V3 => Frame::Map(pairs.into_iter().map(|(f, v)| (bulk(f), bulk(v))).collect()),
        RespVersion::V2 => {
            let mut flat = Vec::with_capacity(pairs.len() * 2);
            for (f, v) in pairs {
                flat.push(bulk(f));
                flat.push(bulk(v));
            }
            Frame::Array(flat)
        }
    }
}

/// `HDEL key field [field ...]` — number of fields actually removed.
fn hdel<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let fields: Vec<Vec<u8>> = args[2..].iter().map(|f| f.to_vec()).collect();
    match ctx.store.hdel(&args[1], &fields) {
        Ok(removed) => Frame::Integer(removed),
        Err(e) => engine_error(e),
    }
}

/// `HLEN key` — field count, `0` for a missing key.
fn hlen<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.hlen(&args[1]) {
        Ok(n) => Frame::Integer(n),
        Err(e) => engine_error(e),
    }
}

/// `HEXISTS key field` — `1`/`0`.
fn hexists<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.hexists(&args[1], &args[2]) {
        Ok(found) => Frame::Integer(i64::from(found)),
        Err(e) => engine_error(e),
    }
}
