//! Connectivity commands: `PING`, `ECHO`.

use bytes::Bytes;
use gamestore_engine::GeneralEngine;
use gamestore_protocol::Frame;

use crate::registry::{wrong_args, CommandRegistry, ExecCtx};

/// Register the connectivity commands.
pub fn register<E: GeneralEngine + 'static>(reg: &mut CommandRegistry<E>) {
    reg.register("PING", -1, ping::<E>);
    reg.register("ECHO", 2, echo::<E>);
}

/// `PING [message]` — `+PONG` without an argument, echoes the bulk otherwise.
fn ping<E: GeneralEngine>(_ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    match args.len() {
        1 => Frame::simple("PONG"),
        2 => Frame::Bulk(args[1].clone()),
        _ => wrong_args("ping"),
    }
}

/// `ECHO message`.
fn echo<E: GeneralEngine>(_ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
    Frame::Bulk(args[1].clone())
}
