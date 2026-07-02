//! List commands: `LPUSH`, `RPUSH`, `LPOP`, `RPOP`, `LRANGE`, `LLEN`
//! (plan I-06).
//!
//! On disk each element is a subkey whose field is a fixed-width big-endian
//! index; the metadata carries the `[head, tail)` bounds so both ends push and
//! pop in O(1) ([`docs/design/03-storage-engine.md`] §2.3).

use bytes::Bytes;
use gamestore_engine::GeneralEngine;
use gamestore_protocol::Frame;

use crate::registry::{
    bulk, engine_error, parse_i64, wrong_args, CommandRegistry, ExecCtx, NOT_AN_INTEGER,
};

/// Register the List commands.
pub fn register<E: GeneralEngine + 'static>(reg: &mut CommandRegistry<E>) {
    reg.register("LPUSH", -3, push::<E>);
    reg.register("RPUSH", -3, push::<E>);
    reg.register("LPOP", -2, pop::<E>);
    reg.register("RPOP", -2, pop::<E>);
    reg.register("LRANGE", 4, lrange::<E>);
    reg.register("LLEN", 2, llen::<E>);
}

/// `LPUSH`/`RPUSH key element [element ...]` — list length after the push.
fn push<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let left = args[0].eq_ignore_ascii_case(b"LPUSH");
    let values: Vec<Vec<u8>> = args[2..].iter().map(|v| v.to_vec()).collect();
    match ctx.store.push(&args[1], &values, left) {
        Ok(len) => Frame::Integer(len),
        Err(e) => engine_error(e),
    }
}

/// `LPOP`/`RPOP key [count]`.
///
/// Without `count`: one element as a bulk string, nil when missing. With
/// `count`: an array of up to `count` elements (`count = 0` gives an empty
/// array), nil when the key is missing — matching Redis 6.2 semantics.
fn pop<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let left = args[0].eq_ignore_ascii_case(b"LPOP");
    let count = match args.get(2) {
        None => None,
        Some(raw) => match parse_i64(raw) {
            Some(n) if n >= 0 => Some(n as usize),
            Some(_) => return Frame::error("ERR value is out of range, must be positive"),
            None => return Frame::error(NOT_AN_INTEGER),
        },
    };
    if args.len() > 3 {
        return wrong_args(if left { "lpop" } else { "rpop" });
    }

    // `count = 0` must not delete/read anything but still distinguishes
    // "key exists -> empty array" from "key missing -> nil".
    let exists = match ctx.store.llen(&args[1]) {
        Ok(n) => n > 0,
        Err(e) => return engine_error(e),
    };
    if !exists {
        return Frame::Null;
    }
    let popped = match ctx.store.pop(&args[1], count.unwrap_or(1), left) {
        Ok(popped) => popped,
        Err(e) => return engine_error(e),
    };
    match count {
        None => popped.into_iter().next().map_or(Frame::Null, bulk),
        Some(_) => Frame::Array(popped.into_iter().map(bulk).collect()),
    }
}

/// `LRANGE key start stop` — elements in the inclusive rank range (negative
/// indexes from the end), empty array when out of range or missing.
fn lrange<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let (Some(start), Some(stop)) = (parse_i64(&args[2]), parse_i64(&args[3])) else {
        return Frame::error(NOT_AN_INTEGER);
    };
    match ctx.store.lrange(&args[1], start, stop) {
        Ok(values) => Frame::Array(values.into_iter().map(bulk).collect()),
        Err(e) => engine_error(e),
    }
}

/// `LLEN key` — list length, `0` for a missing key.
fn llen<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.llen(&args[1]) {
        Ok(n) => Frame::Integer(n),
        Err(e) => engine_error(e),
    }
}
