//! Introspection commands used by the consistency test suite: `DBSIZE`,
//! `RAWCOUNT` (physical subkey records), `COMPACT` (force compaction so the
//! GC filter runs). See plan I-03 and `spike/test/redis_functional_test.py`.
//!
//! Database-wide admin verbs (`FLUSHDB`, …) belong to the DataNode assembly
//! (I-05), not the data model layer.

use bytes::Bytes;
use gamestore_engine::GeneralEngine;
use gamestore_protocol::Frame;

use crate::registry::{engine_error, CommandRegistry, ExecCtx};

/// Register the introspection commands.
pub fn register<E: GeneralEngine + 'static>(reg: &mut CommandRegistry<E>) {
    reg.register("DBSIZE", 1, dbsize::<E>);
    reg.register("RAWCOUNT", 1, rawcount::<E>);
    reg.register("COMPACT", 1, compact::<E>);
}

/// `DBSIZE` — number of live metadata records (logical keys).
fn dbsize<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, _args: &[Bytes]) -> Frame {
    match ctx.store.dbsize() {
        Ok(n) => Frame::Integer(n),
        Err(e) => engine_error(e),
    }
}

/// `RAWCOUNT` — number of physical subkey records (proves GC reclaimed them).
fn rawcount<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, _args: &[Bytes]) -> Frame {
    match ctx.store.raw_subkey_count() {
        Ok(n) => Frame::Integer(n),
        Err(e) => engine_error(e),
    }
}

/// `COMPACT` — force a full compaction so stale records are physically gone.
fn compact<E: GeneralEngine>(ctx: &mut ExecCtx<'_, E>, _args: &[Bytes]) -> Frame {
    match ctx.store.compact() {
        Ok(()) => Frame::ok(),
        Err(e) => engine_error(e),
    }
}
