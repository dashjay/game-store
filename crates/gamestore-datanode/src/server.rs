//! Minimal tokio RESP server for I-01.
//!
//! Accepts TCP connections from standard Redis clients and answers `PING` with
//! `PONG`. This is the seed the single-node MVP grows from: I-05 wires in the
//! command registry, engine and graceful shutdown proper; here we only prove the
//! wiring (tokio accept loop → RESP → reply) works end to end.

use std::future::Future;
use std::io;

use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::resp::{read_command, Reply};

/// Serve connections on `listener` until `shutdown` resolves.
///
/// Each connection is handled on its own task. Errors on individual connections
/// are logged and do not stop the accept loop.
pub async fn serve<S>(listener: TcpListener, shutdown: S) -> io::Result<()>
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
                                tracing::warn!(%peer, error = %e, "connection error");
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
async fn handle_connection(stream: TcpStream) -> io::Result<()> {
    stream.set_nodelay(true).ok();
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    loop {
        match read_command(&mut reader).await? {
            None => break, // clean EOF
            Some(args) if args.is_empty() => continue,
            Some(args) => {
                let (reply, close) = dispatch(&args);
                reply.write_to(&mut write_half).await?;
                write_half.flush().await?;
                if close {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// Dispatch a single command. Returns the reply and whether to close the
/// connection afterwards.
///
/// I-01 supports the handshake/liveness subset a standard Redis client needs to
/// confirm connectivity: `PING`, `ECHO`, `QUIT`. Everything else is an explicit
/// error so unimplemented commands are obvious rather than silently ignored.
fn dispatch(args: &[Vec<u8>]) -> (Reply, bool) {
    let name = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
    match name.as_str() {
        "PING" => match args.len() {
            1 => (Reply::Simple("PONG".to_string()), false),
            2 => (Reply::Bulk(Some(args[1].clone())), false),
            _ => (
                Reply::Error("ERR wrong number of arguments for 'ping' command".to_string()),
                false,
            ),
        },
        "ECHO" => {
            if args.len() == 2 {
                (Reply::Bulk(Some(args[1].clone())), false)
            } else {
                (
                    Reply::Error("ERR wrong number of arguments for 'echo' command".to_string()),
                    false,
                )
            }
        }
        "QUIT" => (Reply::Simple("OK".to_string()), true),
        other => (
            Reply::Error(format!(
                "ERR unknown command '{}' (I-01 DataNode only implements PING/ECHO/QUIT)",
                other.to_ascii_lowercase()
            )),
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_without_arg_returns_pong() {
        let (reply, close) = dispatch(&[b"PING".to_vec()]);
        assert!(matches!(reply, Reply::Simple(s) if s == "PONG"));
        assert!(!close);
    }

    #[test]
    fn ping_is_case_insensitive() {
        let (reply, _) = dispatch(&[b"ping".to_vec()]);
        assert!(matches!(reply, Reply::Simple(s) if s == "PONG"));
    }

    #[test]
    fn ping_with_arg_echoes_it() {
        let (reply, _) = dispatch(&[b"PING".to_vec(), b"hello".to_vec()]);
        assert!(matches!(reply, Reply::Bulk(Some(b)) if b == b"hello"));
    }

    #[test]
    fn quit_closes_connection() {
        let (_, close) = dispatch(&[b"QUIT".to_vec()]);
        assert!(close);
    }

    #[test]
    fn unknown_command_is_an_error() {
        let (reply, _) = dispatch(&[b"GET".to_vec(), b"k".to_vec()]);
        assert!(matches!(reply, Reply::Error(_)));
    }
}
