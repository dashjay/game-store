//! Sorted-set commands: `ZADD`, `ZSCORE`, `ZRANGE`, `ZRANGEBYSCORE`, `ZREM`,
//! `ZCARD` (plan I-06).
//!
//! On disk a ZSet is dual-encoded ([`docs/design/03-storage-engine.md`] §2.3):
//! the member subkey answers `ZSCORE`, a `(score, member)`-ordered score index
//! answers the range scans. This layer parses Redis score syntax (`inf`,
//! exclusive `(` bounds), picks version-appropriate reply shapes (RESP3 gets
//! native doubles / pair arrays) and formats scores the way Redis does.

use bytes::Bytes;
use gamestore_engine::GeneralEngine;
use gamestore_protocol::{Frame, RespVersion};

use crate::registry::{bulk, engine_error, parse_i64, CommandRegistry, ExecCtx, NOT_AN_INTEGER};

/// Register the ZSet commands.
pub fn register<E: GeneralEngine + 'static>(reg: &mut CommandRegistry<E>) {
    reg.register("ZADD", -4, zadd::<E>);
    reg.register("ZSCORE", 3, zscore::<E>);
    reg.register("ZRANGE", -4, zrange::<E>);
    reg.register("ZRANGEBYSCORE", -4, zrangebyscore::<E>);
    reg.register("ZREM", -3, zrem::<E>);
    reg.register("ZCARD", 2, zcard::<E>);
}

const NOT_A_FLOAT: &str = "ERR value is not a valid float";
const BAD_RANGE_BOUND: &str = "ERR min or max is not a float";

/// Parse a Redis score: a finite decimal float or an (optionally signed)
/// infinity. `NaN` and non-numeric input are rejected.
fn parse_score(raw: &[u8]) -> Option<f64> {
    let s = std::str::from_utf8(raw).ok()?;
    if s.is_empty() || s.chars().any(|c| c.is_whitespace()) {
        return None;
    }
    let v: f64 = s.parse().ok()?;
    if v.is_nan() {
        return None;
    }
    Some(v)
}

/// Parse a `ZRANGEBYSCORE` bound: a score, optionally prefixed with `(` for
/// exclusive. Returns `(value, exclusive)`.
fn parse_score_bound(raw: &[u8]) -> Option<(f64, bool)> {
    match raw.split_first() {
        Some((b'(', rest)) => parse_score(rest).map(|v| (v, true)),
        _ => parse_score(raw).map(|v| (v, false)),
    }
}

/// Format a score the way Redis replies: integral values without a decimal
/// point, infinities as `inf`/`-inf`, everything else in the shortest form
/// that round-trips through an `f64`.
fn format_score(score: f64) -> String {
    if score == score.trunc() && score.is_finite() && score.abs() < 1e17 {
        format!("{}", score as i64)
    } else {
        format!("{score}")
    }
}

/// A score reply: RESP3 clients get a native double, RESP2 a bulk string.
fn score_frame(score: f64, version: RespVersion) -> Frame {
    match version {
        RespVersion::V3 => Frame::Double(score),
        RespVersion::V2 => Frame::Bulk(Bytes::from(format_score(score))),
    }
}

/// Shape a `(member, score)` listing for the wire: without `WITHSCORES` a
/// plain member array; with it RESP2 flattens `member, score, ...` while RESP3
/// nests `[member, score]` pairs with native doubles (matching Redis 7).
fn range_reply(pairs: Vec<(Vec<u8>, f64)>, withscores: bool, version: RespVersion) -> Frame {
    if !withscores {
        return Frame::Array(pairs.into_iter().map(|(m, _)| bulk(m)).collect());
    }
    match version {
        RespVersion::V2 => {
            let mut flat = Vec::with_capacity(pairs.len() * 2);
            for (member, score) in pairs {
                flat.push(bulk(member));
                flat.push(Frame::Bulk(Bytes::from(format_score(score))));
            }
            Frame::Array(flat)
        }
        RespVersion::V3 => Frame::Array(
            pairs
                .into_iter()
                .map(|(member, score)| Frame::Array(vec![bulk(member), Frame::Double(score)]))
                .collect(),
        ),
    }
}

/// `ZADD key score member [score member ...]`.
///
/// Replies with the number of *new* members. The conditional/return-modifying
/// flags (`NX`/`XX`/`GT`/`LT`/`CH`/`INCR`) are out of I-06 scope and rejected
/// as syntax errors (same precedent as `SET`'s `NX`/`XX` in I-04).
fn zadd<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    const FLAGS: &[&[u8]] = &[b"NX", b"XX", b"GT", b"LT", b"CH", b"INCR"];
    if FLAGS.iter().any(|f| args[2].eq_ignore_ascii_case(f)) {
        return Frame::error("ERR syntax error");
    }
    if (args.len() - 2) % 2 != 0 {
        return Frame::error("ERR syntax error");
    }
    let mut pairs = Vec::with_capacity((args.len() - 2) / 2);
    for sm in args[2..].chunks_exact(2) {
        let Some(score) = parse_score(&sm[0]) else {
            return Frame::error(NOT_A_FLOAT);
        };
        pairs.push((score, sm[1].to_vec()));
    }
    match ctx.store.zadd(&args[1], &pairs) {
        Ok(added) => Frame::Integer(added),
        Err(e) => engine_error(e),
    }
}

/// `ZSCORE key member` — the member's score, nil when key/member is missing.
fn zscore<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.zscore(&args[1], &args[2]) {
        Ok(Some(score)) => score_frame(score, ctx.version),
        Ok(None) => Frame::Null,
        Err(e) => engine_error(e),
    }
}

/// `ZRANGE key start stop [WITHSCORES]` — members by ascending rank.
///
/// The Redis 6.2 extensions (`BYSCORE`/`BYLEX`/`REV`/`LIMIT`) are out of I-06
/// scope (`ZRANGEBYSCORE` covers the score form) and rejected as syntax errors.
fn zrange<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let (Some(start), Some(stop)) = (parse_i64(&args[2]), parse_i64(&args[3])) else {
        return Frame::error(NOT_AN_INTEGER);
    };
    let withscores = match args.get(4) {
        None => false,
        Some(opt) if opt.eq_ignore_ascii_case(b"WITHSCORES") => true,
        Some(_) => return Frame::error("ERR syntax error"),
    };
    if args.len() > 5 {
        return Frame::error("ERR syntax error");
    }
    match ctx.store.zrange(&args[1], start, stop) {
        Ok(pairs) => range_reply(pairs, withscores, ctx.version),
        Err(e) => engine_error(e),
    }
}

/// `ZRANGEBYSCORE key min max [WITHSCORES] [LIMIT offset count]` — members
/// whose score falls within the (optionally exclusive, optionally infinite)
/// bounds, by ascending score.
fn zrangebyscore<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let Some((min, min_excl)) = parse_score_bound(&args[2]) else {
        return Frame::error(BAD_RANGE_BOUND);
    };
    let Some((max, max_excl)) = parse_score_bound(&args[3]) else {
        return Frame::error(BAD_RANGE_BOUND);
    };

    let mut withscores = false;
    let mut limit: Option<(i64, i64)> = None;
    let mut i = 4;
    while i < args.len() {
        if args[i].eq_ignore_ascii_case(b"WITHSCORES") {
            withscores = true;
            i += 1;
        } else if args[i].eq_ignore_ascii_case(b"LIMIT") {
            let (Some(off), Some(cnt)) = (
                args.get(i + 1).and_then(|b| parse_i64(b)),
                args.get(i + 2).and_then(|b| parse_i64(b)),
            ) else {
                return Frame::error("ERR syntax error");
            };
            limit = Some((off, cnt));
            i += 3;
        } else {
            return Frame::error("ERR syntax error");
        }
    }

    let pairs = match ctx
        .store
        .zrange_by_score(&args[1], min, min_excl, max, max_excl)
    {
        Ok(pairs) => pairs,
        Err(e) => return engine_error(e),
    };
    let pairs = match limit {
        // A negative offset yields nothing; a negative count means "no limit"
        // (all elements from the offset), matching Redis.
        Some((off, _)) if off < 0 => Vec::new(),
        Some((off, cnt)) => {
            let iter = pairs.into_iter().skip(off as usize);
            if cnt < 0 {
                iter.collect()
            } else {
                iter.take(cnt as usize).collect()
            }
        }
        None => pairs,
    };
    range_reply(pairs, withscores, ctx.version)
}

/// `ZREM key member [member ...]` — number of members actually removed.
fn zrem<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let members: Vec<Vec<u8>> = args[2..].iter().map(|m| m.to_vec()).collect();
    match ctx.store.zrem(&args[1], &members) {
        Ok(removed) => Frame::Integer(removed),
        Err(e) => engine_error(e),
    }
}

/// `ZCARD key` — member count, `0` for a missing key.
fn zcard<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.zcard(&args[1]) {
        Ok(n) => Frame::Integer(n),
        Err(e) => engine_error(e),
    }
}
