//! String and generic key/TTL commands: `SET` (with `EX`/`PX`), `GET`, `DEL`,
//! `EXISTS`, `TYPE`, `EXPIRE`, `PEXPIRE`, `TTL`, `PTTL`.
//!
//! Expiry is stored in the engine as an **absolute** unix-epoch millisecond
//! timestamp and enforced lazily on access ([`docs/design/03-storage-engine.md`]
//! §3). This layer converts the relative seconds/milliseconds a client sends
//! into that absolute deadline, and converts the engine's `pttl` (remaining
//! milliseconds) back into seconds — rounded **up**, matching Redis — for `TTL`.

use bytes::Bytes;
use gamestore_engine::{now_ms, GeneralEngine};
use gamestore_protocol::Frame;

use crate::registry::{engine_error, parse_i64, CommandRegistry, ExecCtx, NOT_AN_INTEGER};

/// Register the String + generic TTL commands.
pub fn register<E: GeneralEngine + 'static>(reg: &mut CommandRegistry<E>) {
    reg.register("SET", -3, set::<E>);
    reg.register("GET", 2, get::<E>);
    reg.register("DEL", -2, del::<E>);
    reg.register("EXISTS", -2, exists::<E>);
    reg.register("TYPE", 2, type_of::<E>);
    reg.register("EXPIRE", 3, expire::<E>);
    reg.register("PEXPIRE", 3, pexpire::<E>);
    reg.register("TTL", 2, ttl::<E>);
    reg.register("PTTL", 2, pttl::<E>);
}

fn invalid_expire(cmd: &str) -> Frame {
    Frame::error(format!("ERR invalid expire time in '{cmd}' command"))
}

/// `SET key value [EX seconds | PX milliseconds]`.
///
/// Redis semantics for the expire options: the argument must be a positive
/// integer (`ERR invalid expire time` otherwise) and `EX`/`PX` are mutually
/// exclusive (`ERR syntax error`). Other `SET` options (`NX`/`XX`/`KEEPTTL`/
/// `GET`) are out of I-04 scope and rejected as syntax errors.
fn set<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let mut expire_ms = 0u64;
    let mut i = 3;
    while i < args.len() {
        let unit_ms: i64 = if args[i].eq_ignore_ascii_case(b"EX") {
            1000
        } else if args[i].eq_ignore_ascii_case(b"PX") {
            1
        } else {
            return Frame::error("ERR syntax error");
        };
        if expire_ms != 0 {
            // A second expire option (EX + PX) is a syntax error in Redis.
            return Frame::error("ERR syntax error");
        }
        let Some(raw) = args.get(i + 1) else {
            return Frame::error("ERR syntax error");
        };
        let Some(n) = parse_i64(raw) else {
            return Frame::error(NOT_AN_INTEGER);
        };
        if n <= 0 {
            return invalid_expire("set");
        }
        let Some(deadline) = n
            .checked_mul(unit_ms)
            .and_then(|delta| delta.checked_add(now_ms() as i64))
        else {
            return invalid_expire("set");
        };
        expire_ms = deadline as u64;
        i += 2;
    }
    match ctx.store.set(&args[1], &args[2], expire_ms) {
        Ok(()) => Frame::ok(),
        Err(e) => engine_error(e),
    }
}

/// `GET key` — nil when missing, `WRONGTYPE` when the key is not a String.
fn get<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.get(&args[1]) {
        Ok(Some(v)) => Frame::Bulk(v.into()),
        Ok(None) => Frame::Null,
        Err(e) => engine_error(e),
    }
}

/// `DEL key [key ...]` — number of keys actually removed.
fn del<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let mut removed = 0i64;
    for key in &args[1..] {
        match ctx.store.del(key) {
            Ok(true) => removed += 1,
            Ok(false) => {}
            Err(e) => return engine_error(e),
        }
    }
    Frame::Integer(removed)
}

/// `EXISTS key [key ...]` — counts every existing argument (duplicates counted
/// once per mention, like Redis).
fn exists<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let mut n = 0i64;
    for key in &args[1..] {
        match ctx.store.exists(key) {
            Ok(true) => n += 1,
            Ok(false) => {}
            Err(e) => return engine_error(e),
        }
    }
    Frame::Integer(n)
}

/// `TYPE key` — `string` / `hash` / `none` as a simple string.
fn type_of<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.type_of(&args[1]) {
        Ok(t) => Frame::simple(t),
        Err(e) => engine_error(e),
    }
}

/// `EXPIRE key seconds`.
fn expire<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    expire_generic(ctx, args, 1000, "expire")
}

/// `PEXPIRE key milliseconds`.
fn pexpire<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    expire_generic(ctx, args, 1, "pexpire")
}

/// Shared `EXPIRE`/`PEXPIRE` body: converts the relative amount (in `unit_ms`
/// units) into an absolute deadline for [`Store::expire_at`], replying `1` if
/// the key exists and `0` otherwise.
///
/// A non-positive amount is valid in Redis and deletes the key: we clamp the
/// deadline to `1` (an epoch timestamp long past), which the engine's lazy
/// expiry treats as already gone — while still replying `1` because the key
/// existed when the command ran.
///
/// [`Store::expire_at`]: gamestore_engine::Store::expire_at
fn expire_generic<E: GeneralEngine>(
    ctx: &mut ExecCtx<'_, E>,
    args: &[Bytes],
    unit_ms: i64,
    cmd: &str,
) -> Frame {
    let Some(n) = parse_i64(&args[2]) else {
        return Frame::error(NOT_AN_INTEGER);
    };
    let Some(delta) = n.checked_mul(unit_ms) else {
        return invalid_expire(cmd);
    };
    // `expire_ms == 0` means "no expiry" in the engine encoding, so an
    // already-past deadline is clamped to 1ms-after-epoch instead.
    let deadline = (now_ms() as i64).saturating_add(delta).max(1) as u64;
    match ctx.store.expire_at(&args[1], deadline) {
        Ok(existed) => Frame::Integer(i64::from(existed)),
        Err(e) => engine_error(e),
    }
}

/// `TTL key` — remaining seconds rounded up; `-1` no expiry, `-2` missing.
fn ttl<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.pttl(&args[1]) {
        Ok(ms) if ms < 0 => Frame::Integer(ms),
        Ok(ms) => Frame::Integer((ms + 999) / 1000),
        Err(e) => engine_error(e),
    }
}

/// `PTTL key` — remaining milliseconds; `-1` no expiry, `-2` missing.
fn pttl<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.pttl(&args[1]) {
        Ok(ms) => Frame::Integer(ms),
        Err(e) => engine_error(e),
    }
}
