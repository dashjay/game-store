//! Set commands: `SADD`, `SREM`, `SISMEMBER`, `SMEMBERS`, `SCARD` (plan I-06).
//!
//! On disk each member is a subkey with an empty value under the owner's
//! current structure version — membership is expressed by the key itself
//! ([`docs/design/03-storage-engine.md`] §2.3).

use bytes::Bytes;
use gamestore_engine::GeneralEngine;
use gamestore_protocol::{Frame, RespVersion};

use crate::registry::{bulk, engine_error, CommandRegistry, ExecCtx};

/// Register the Set commands.
pub fn register<E: GeneralEngine + 'static>(reg: &mut CommandRegistry<E>) {
    reg.register("SADD", -3, sadd::<E>);
    reg.register("SREM", -3, srem::<E>);
    reg.register("SISMEMBER", 3, sismember::<E>);
    reg.register("SMEMBERS", 2, smembers::<E>);
    reg.register("SCARD", 2, scard::<E>);
}

/// `SADD key member [member ...]` — number of members actually added.
fn sadd<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let members: Vec<Vec<u8>> = args[2..].iter().map(|m| m.to_vec()).collect();
    match ctx.store.sadd(&args[1], &members) {
        Ok(added) => Frame::Integer(added),
        Err(e) => engine_error(e),
    }
}

/// `SREM key member [member ...]` — number of members actually removed.
fn srem<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let members: Vec<Vec<u8>> = args[2..].iter().map(|m| m.to_vec()).collect();
    match ctx.store.srem(&args[1], &members) {
        Ok(removed) => Frame::Integer(removed),
        Err(e) => engine_error(e),
    }
}

/// `SISMEMBER key member` — `1`/`0` (missing key counts as `0`).
fn sismember<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.sismember(&args[1], &args[2]) {
        Ok(found) => Frame::Integer(i64::from(found)),
        Err(e) => engine_error(e),
    }
}

/// `SMEMBERS key` — RESP3 connections get a native set, RESP2 the classic
/// array. Empty reply for a missing key, like Redis.
fn smembers<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    let members = match ctx.store.smembers(&args[1]) {
        Ok(members) => members,
        Err(e) => return engine_error(e),
    };
    let items: Vec<Frame> = members.into_iter().map(bulk).collect();
    match ctx.version {
        RespVersion::V3 => Frame::Set(items),
        RespVersion::V2 => Frame::Array(items),
    }
}

/// `SCARD key` — member count, `0` for a missing key.
fn scard<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match ctx.store.scard(&args[1]) {
        Ok(n) => Frame::Integer(n),
        Err(e) => engine_error(e),
    }
}
