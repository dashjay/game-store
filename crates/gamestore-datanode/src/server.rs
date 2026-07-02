//! tokio RESP connection service.
//!
//! Since **I-02** the wire handling is delegated to the [`gamestore_protocol`]
//! codec: each connection is a [`Framed`] stream of client requests
//! (`Vec<Bytes>`) with [`Frame`] replies. Protocol version (RESP2/RESP3) is
//! negotiated per connection via `HELLO`.
//!
//! The command surface is still deliberately small — the handshake/liveness
//! subset a standard Redis client needs to connect: `PING`, `ECHO`, `HELLO`,
//! `QUIT`. The full command registry + storage engine arrive in I-04/I-05; this
//! module proves the accept-loop → codec → dispatch → reply path end to end.

use std::future::Future;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gamestore_protocol::{CommandCodec, Frame, RespVersion};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

/// Serve connections on `listener` until `shutdown` resolves.
///
/// Each connection is handled on its own task. Errors on individual connections
/// are logged and do not stop the accept loop.
pub async fn serve<S>(listener: TcpListener, shutdown: S) -> std::io::Result<()>
where
    S: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                tracing::info!("shutdown signal received, stopping accept loop");
                return Ok(());
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        tracing::debug!(%peer, "connection accepted");
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream).await {
                                tracing::warn!(%peer, error = %e, "connection closed with error");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept error");
                    }
                }
            }
        }
    }
}

/// Per-connection read/dispatch/reply loop.
async fn handle_connection(stream: TcpStream) -> Result<(), gamestore_protocol::CodecError> {
    stream.set_nodelay(true).ok();
    let mut framed = Framed::new(stream, CommandCodec::new());

    while let Some(item) = framed.next().await {
        let args = match item {
            Ok(args) => args,
            Err(e) => {
                // Best-effort: tell the client it desynced, then drop the conn.
                let _ = framed
                    .send(Frame::error(format!("ERR Protocol error: {e}")))
                    .await;
                return Err(e);
            }
        };
        if args.is_empty() {
            continue;
        }

        let action = dispatch(&args, framed.codec().version());
        // Switch the encoder *before* replying so e.g. `HELLO 3`'s own reply is
        // already RESP3.
        if let Some(v) = action.set_version {
            framed.codec_mut().set_version(v);
        }
        framed.send(action.reply).await?;
        if action.close {
            break;
        }
    }
    Ok(())
}

/// The result of dispatching one command.
struct Action {
    reply: Frame,
    close: bool,
    set_version: Option<RespVersion>,
}

impl Action {
    fn reply(reply: Frame) -> Self {
        Action {
            reply,
            close: false,
            set_version: None,
        }
    }

    fn close_with(reply: Frame) -> Self {
        Action {
            reply,
            close: true,
            set_version: None,
        }
    }
}

/// Dispatch a single command given the connection's current protocol version.
///
/// I-02 implements the handshake/liveness subset; everything else is an explicit
/// error so unimplemented commands are obvious rather than silently ignored.
fn dispatch(args: &[Bytes], version: RespVersion) -> Action {
    let name = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
    match name.as_str() {
        "PING" => match args.len() {
            1 => Action::reply(Frame::simple("PONG")),
            2 => Action::reply(Frame::Bulk(args[1].clone())),
            _ => Action::reply(wrong_args("ping")),
        },
        "ECHO" => {
            if args.len() == 2 {
                Action::reply(Frame::Bulk(args[1].clone()))
            } else {
                Action::reply(wrong_args("echo"))
            }
        }
        "HELLO" => hello(args, version),
        "QUIT" => Action::close_with(Frame::ok()),
        other => Action::reply(Frame::error(format!(
            "ERR unknown command '{}' (I-02 DataNode implements PING/ECHO/HELLO/QUIT)",
            other.to_ascii_lowercase()
        ))),
    }
}

/// Handle `HELLO [protover [AUTH user pass] [SETNAME name]]`.
///
/// We honor the requested protocol version (2 or 3) and reply with the standard
/// server-info map. Auth/SETNAME options are tolerated but ignored in I-02 (no
/// auth yet, no client-name tracking). An unsupported version yields `NOPROTO`.
fn hello(args: &[Bytes], current: RespVersion) -> Action {
    let mut target = current;
    let mut set_version = None;
    if args.len() >= 2 {
        let raw = String::from_utf8_lossy(&args[1]);
        match raw
            .trim()
            .parse::<i64>()
            .ok()
            .and_then(RespVersion::from_i64)
        {
            Some(v) => {
                target = v;
                set_version = Some(v);
            }
            None => {
                return Action::reply(Frame::error(
                    "NOPROTO unsupported protocol version".to_string(),
                ));
            }
        }
    }

    let reply = hello_reply(target);
    Action {
        reply,
        close: false,
        set_version,
    }
}

/// Build the `HELLO` server-info reply. RESP3 uses a map; RESP2 flattens it into
/// an array of alternating key/value entries (matching Redis).
fn hello_reply(version: RespVersion) -> Frame {
    let fields: Vec<(Frame, Frame)> = vec![
        (Frame::from("server"), Frame::from("gamestore")),
        (
            Frame::from("version"),
            Frame::from(env!("CARGO_PKG_VERSION")),
        ),
        (Frame::from("proto"), Frame::Integer(version.as_i64())),
        (Frame::from("id"), Frame::Integer(0)),
        (Frame::from("mode"), Frame::from("standalone")),
        (Frame::from("role"), Frame::from("master")),
        (Frame::from("modules"), Frame::Array(vec![])),
    ];
    match version {
        RespVersion::V3 => Frame::Map(fields),
        RespVersion::V2 => {
            let mut flat = Vec::with_capacity(fields.len() * 2);
            for (k, v) in fields {
                flat.push(k);
                flat.push(v);
            }
            Frame::Array(flat)
        }
    }
}

fn wrong_args(cmd: &str) -> Frame {
    Frame::error(format!("ERR wrong number of arguments for '{cmd}' command"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(parts: &[&str]) -> Vec<Bytes> {
        parts
            .iter()
            .map(|s| Bytes::from(s.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn ping_without_arg_returns_pong() {
        let action = dispatch(&a(&["PING"]), RespVersion::V2);
        assert!(matches!(action.reply, Frame::Simple(s) if s == "PONG"));
        assert!(!action.close);
    }

    #[test]
    fn ping_is_case_insensitive() {
        let action = dispatch(&a(&["ping"]), RespVersion::V2);
        assert!(matches!(action.reply, Frame::Simple(s) if s == "PONG"));
    }

    #[test]
    fn ping_with_arg_echoes_it() {
        let action = dispatch(&a(&["PING", "hello"]), RespVersion::V2);
        assert!(matches!(action.reply, Frame::Bulk(b) if b == "hello".as_bytes()));
    }

    #[test]
    fn echo_requires_one_arg() {
        assert!(matches!(
            dispatch(&a(&["ECHO"]), RespVersion::V2).reply,
            Frame::Error(_)
        ));
        assert!(matches!(
            dispatch(&a(&["ECHO", "hi"]), RespVersion::V2).reply,
            Frame::Bulk(b) if b == "hi".as_bytes()
        ));
    }

    #[test]
    fn quit_closes_connection() {
        let action = dispatch(&a(&["QUIT"]), RespVersion::V2);
        assert!(action.close);
    }

    #[test]
    fn unknown_command_is_an_error() {
        let action = dispatch(&a(&["GET", "k"]), RespVersion::V2);
        assert!(matches!(action.reply, Frame::Error(_)));
    }

    #[test]
    fn hello_without_arg_keeps_version_and_replies_array_in_resp2() {
        let action = dispatch(&a(&["HELLO"]), RespVersion::V2);
        assert!(action.set_version.is_none());
        assert!(matches!(action.reply, Frame::Array(_)));
    }

    #[test]
    fn hello_3_switches_to_resp3_and_replies_map() {
        let action = dispatch(&a(&["HELLO", "3"]), RespVersion::V2);
        assert_eq!(action.set_version, Some(RespVersion::V3));
        assert!(matches!(action.reply, Frame::Map(_)));
    }

    #[test]
    fn hello_2_stays_resp2() {
        let action = dispatch(&a(&["HELLO", "2"]), RespVersion::V3);
        assert_eq!(action.set_version, Some(RespVersion::V2));
        assert!(matches!(action.reply, Frame::Array(_)));
    }

    #[test]
    fn hello_bad_version_is_noproto() {
        let action = dispatch(&a(&["HELLO", "9"]), RespVersion::V2);
        assert!(action.set_version.is_none());
        assert!(matches!(action.reply, Frame::Error(e) if e.starts_with("NOPROTO")));
    }
}
