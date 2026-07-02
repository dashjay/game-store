//! tokio RESP connection service (I-05: single-node service assembly).
//!
//! Since **I-02** the wire handling is delegated to the [`gamestore_protocol`]
//! codec: each connection is a [`Framed`] stream of client requests
//! (`Vec<Bytes>`) with [`Frame`] replies, and the protocol version
//! (RESP2/RESP3) is negotiated per connection via `HELLO`.
//!
//! As of **I-05** every data command is dispatched through the
//! [`gamestore_datamodel::CommandRegistry`] against a shared
//! [`Store`] (`Arc<Store<E>>`, one store per DataNode). Only
//! **connection-scoped** commands (`HELLO`, `QUIT`, plus the `CLIENT`/
//! `SELECT`/`COMMAND` housekeeping a standard client emits) and **database
//! admin** verbs (`FLUSHDB`/`FLUSHALL`) are handled here in the DataNode
//! layer — they concern the connection/server, not the data model.
//!
//! # Blocking engine calls on the tokio runtime
//!
//! Engine calls are executed **synchronously inline** on the runtime worker
//! rather than through `spawn_blocking`. Rationale (recorded in
//! `docs/EVOLUTION.md` MR-0018): Phase-1 operations are RocksDB point
//! reads/writes and short prefix scans — microsecond-scale against memtable/
//! block cache, without fsync on the foreground path — while `spawn_blocking`
//! adds task-handoff latency to *every* command and funnels work through the
//! (unbounded, cold) blocking pool. The multi-threaded runtime keeps other
//! connections progressing on their own workers. The one genuinely long call
//! (`COMPACT`) is a test/admin introspection verb, not a hot-path command.
//! **Re-reviewed in I-07 with benchmark data and upheld** (see
//! `docs/benchmarks/2026-07-02-i07-baseline.md` §3): hot-path commands
//! execute in 0.4–7.4 µs, the same order as a `spawn_blocking` handoff
//! itself; the heaviest range command stays ~100 µs. Revisit when a hot-path
//! command reaches millisecond scale or when the WAL (I-08) puts fsync on the
//! foreground write path.
//!
//! # `Core` as a logical unit (reservation, not implemented)
//!
//! The target DataNode form (`docs/design/02-architecture.md` §3.2) shards a
//! node into **Cores**: one pinned thread per core, run-to-complete, multiple
//! Replicas per Core sharing one WAL. Per plan §1 we deliberately do *not*
//! implement thread-per-core in Phase 1. The logical placeholder is that a
//! DataNode today runs exactly **one Core** — one shared `Store` behind an
//! `Arc`, served by the tokio multi-thread runtime. When I-08 (WAL) and the
//! Phase-2 replication MRs arrive, this single `Arc<Store>` becomes a
//! `Core` unit (store + WAL + replica set), and `serve` grows a `Vec<Core>`
//! routed by partition — without changing the connection/dispatch layering
//! established here.

use std::future::Future;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gamestore_datamodel::{CommandRegistry, ExecCtx};
use gamestore_engine::{GeneralEngine, Store};
use gamestore_protocol::{CommandCodec, Frame, RespVersion};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_util::codec::Framed;

use crate::observability;

/// Monotonic connection-id source (`CLIENT ID`, `HELLO` reply `id` field).
static NEXT_CONN_ID: AtomicI64 = AtomicI64::new(1);

/// Tunables for [`serve_with`] (I-07).
#[derive(Debug, Clone)]
pub struct ServeOptions {
    /// Commands at or above this duration are reported to the slow log and
    /// counted in `datanode_slow_commands_total`.
    pub slow_log_threshold: Duration,
}

impl Default for ServeOptions {
    fn default() -> Self {
        ServeOptions {
            // Redis's `slowlog-log-slower-than` default (10'000 µs).
            slow_log_threshold: Duration::from_millis(10),
        }
    }
}

/// [`serve_with`] using default [`ServeOptions`].
pub async fn serve<E, S>(
    listener: TcpListener,
    store: Arc<Store<E>>,
    shutdown: S,
) -> std::io::Result<()>
where
    E: GeneralEngine + 'static,
    S: Future<Output = ()>,
{
    serve_with(listener, store, ServeOptions::default(), shutdown).await
}

/// Serve connections on `listener` against `store` until `shutdown` resolves.
///
/// Each connection is handled on its own task; errors on individual
/// connections are logged and do not stop the accept loop. On shutdown the
/// accept loop stops, every open connection is signalled to finish its
/// in-flight command and close, and `serve` waits for all of them to drain
/// before returning (graceful shutdown).
pub async fn serve_with<E, S>(
    listener: TcpListener,
    store: Arc<Store<E>>,
    options: ServeOptions,
    shutdown: S,
) -> std::io::Result<()>
where
    E: GeneralEngine + 'static,
    S: Future<Output = ()>,
{
    let registry = Arc::new(CommandRegistry::<E>::standard());
    let options = Arc::new(options);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let mut connections = JoinSet::new();

    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                tracing::info!("shutdown signal received, stopping accept loop");
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, peer)) => {
                        tracing::debug!(%peer, "connection accepted");
                        let store = store.clone();
                        let registry = registry.clone();
                        let options = options.clone();
                        let rx = shutdown_rx.clone();
                        connections.spawn(async move {
                            metrics::gauge!("datanode_conn_active").increment(1.0);
                            if let Err(e) =
                                handle_connection(stream, store, registry, options, rx).await
                            {
                                tracing::warn!(%peer, error = %e, "connection closed with error");
                            }
                            metrics::gauge!("datanode_conn_active").decrement(1.0);
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept error");
                    }
                }
            }
        }
    }

    // Graceful drain: tell every connection to stop after its in-flight
    // command, then wait for all of them.
    let _ = shutdown_tx.send(true);
    let open = connections.len();
    if open > 0 {
        tracing::info!(connections = open, "draining open connections");
    }
    while connections.join_next().await.is_some() {}
    Ok(())
}

/// Per-connection read/dispatch/reply loop.
async fn handle_connection<E: GeneralEngine + 'static>(
    stream: TcpStream,
    store: Arc<Store<E>>,
    registry: Arc<CommandRegistry<E>>,
    options: Arc<ServeOptions>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), gamestore_protocol::CodecError> {
    stream.set_nodelay(true).ok();
    let mut framed = Framed::new(stream, CommandCodec::new());
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);

    loop {
        let item = tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            item = framed.next() => match item {
                Some(item) => item,
                None => break, // client closed
            },
        };
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

        let started = Instant::now();
        let action = dispatch(&args, framed.codec().version(), conn_id, &store, &registry);
        observability::record_command(
            command_label(&args[0], &registry),
            &args,
            started.elapsed(),
            options.slow_log_threshold,
        );
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

/// Connection-scoped verbs handled by the DataNode layer itself (everything
/// else known lives in the [`CommandRegistry`]).
const CONNECTION_VERBS: &[&str] = &[
    "HELLO", "QUIT", "CLIENT", "SELECT", "COMMAND", "FLUSHDB", "FLUSHALL",
];

/// The metric label for a command: its canonical uppercase name when it is a
/// known command, `"UNKNOWN"` otherwise — arbitrary client input must never
/// mint new label values (bounded cardinality).
fn command_label<E: GeneralEngine + 'static>(raw: &Bytes, registry: &CommandRegistry<E>) -> String {
    let upper = String::from_utf8_lossy(raw).to_ascii_uppercase();
    if CONNECTION_VERBS.contains(&upper.as_str()) || registry.contains(raw) {
        upper
    } else {
        "UNKNOWN".to_string()
    }
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
/// Connection-scoped commands (`HELLO`/`QUIT` + `CLIENT`/`SELECT`/`COMMAND`
/// housekeeping) and database admin (`FLUSHDB`/`FLUSHALL`) are handled here;
/// everything else goes through the [`CommandRegistry`], which owns the
/// canonical `ERR unknown command '...'` wording for the rest.
fn dispatch<E: GeneralEngine + 'static>(
    args: &[Bytes],
    version: RespVersion,
    conn_id: i64,
    store: &Store<E>,
    registry: &CommandRegistry<E>,
) -> Action {
    let name = String::from_utf8_lossy(&args[0]).to_ascii_uppercase();
    match name.as_str() {
        "HELLO" => hello(args, version, conn_id),
        "QUIT" => Action::close_with(Frame::ok()),
        "CLIENT" => Action::reply(client(args, conn_id)),
        "SELECT" => Action::reply(select(args)),
        "COMMAND" => Action::reply(Frame::Array(vec![])),
        "FLUSHDB" | "FLUSHALL" => Action::reply(flush(args, store)),
        _ => {
            let mut ctx = ExecCtx::new(store, version);
            Action::reply(registry.dispatch(&mut ctx, args))
        }
    }
}

/// Handle `FLUSHDB` / `FLUSHALL [ASYNC|SYNC]`.
///
/// There is a single (implicit) database in GameStore, so both verbs clear the
/// whole store — metadata, subkeys *and* the in-memory version table (see
/// [`Store::flush_all`]). The `ASYNC`/`SYNC` modifiers are accepted and both
/// run synchronously (the flush itself is a bounded scan+delete batch).
fn flush<E: GeneralEngine>(args: &[Bytes], store: &Store<E>) -> Frame {
    let name = if args[0].eq_ignore_ascii_case(b"FLUSHALL") {
        "flushall"
    } else {
        "flushdb"
    };
    match args.len() {
        1 => {}
        2 if args[1].eq_ignore_ascii_case(b"ASYNC") || args[1].eq_ignore_ascii_case(b"SYNC") => {}
        2 => return Frame::error("ERR syntax error"),
        _ => {
            return Frame::error(format!(
                "ERR wrong number of arguments for '{name}' command"
            ))
        }
    }
    match store.flush_all() {
        Ok(()) => Frame::ok(),
        Err(e) => Frame::error(format!("ERR {e}")),
    }
}

/// Handle the `CLIENT` housekeeping subcommands standard clients emit on
/// connect (redis-py sends `CLIENT SETINFO`, others send `CLIENT SETNAME`).
///
/// We acknowledge the setters without tracking the values (no client registry
/// yet), answer `ID`/`GETNAME` honestly, and reject unknown subcommands with
/// Redis's wording so misuse stays visible.
fn client(args: &[Bytes], conn_id: i64) -> Frame {
    let Some(sub) = args.get(1) else {
        return Frame::error("ERR wrong number of arguments for 'client' command");
    };
    let sub_upper = String::from_utf8_lossy(sub).to_ascii_uppercase();
    match sub_upper.as_str() {
        "SETINFO" | "SETNAME" | "NO-EVICT" | "NO-TOUCH" => Frame::ok(),
        "ID" => Frame::Integer(conn_id),
        "GETNAME" => Frame::Bulk(Bytes::new()),
        other => Frame::error(format!(
            "ERR Unknown subcommand or wrong number of arguments for '{other}'. Try CLIENT HELP."
        )),
    }
}

/// Handle `SELECT index`. GameStore has a single logical database, so only
/// index `0` is valid — matching how Redis Cluster answers `SELECT`.
fn select(args: &[Bytes]) -> Frame {
    if args.len() != 2 {
        return Frame::error("ERR wrong number of arguments for 'select' command");
    }
    match std::str::from_utf8(&args[1])
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
    {
        Some(0) => Frame::ok(),
        Some(_) => Frame::error("ERR DB index is out of range"),
        None => Frame::error("ERR value is not an integer or out of range"),
    }
}

/// Handle `HELLO [protover [AUTH user pass] [SETNAME name]]`.
///
/// We honor the requested protocol version (2 or 3) and reply with the
/// standard server-info map. Auth/SETNAME options are tolerated but ignored
/// (no auth yet, no client-name tracking). An unsupported version yields
/// `NOPROTO`.
fn hello(args: &[Bytes], current: RespVersion, conn_id: i64) -> Action {
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

    let reply = hello_reply(target, conn_id);
    Action {
        reply,
        close: false,
        set_version,
    }
}

/// Build the `HELLO` server-info reply. RESP3 uses a map; RESP2 flattens it into
/// an array of alternating key/value entries (matching Redis).
fn hello_reply(version: RespVersion, conn_id: i64) -> Frame {
    let fields: Vec<(Frame, Frame)> = vec![
        (Frame::from("server"), Frame::from("gamestore")),
        (
            Frame::from("version"),
            Frame::from(env!("CARGO_PKG_VERSION")),
        ),
        (Frame::from("proto"), Frame::Integer(version.as_i64())),
        (Frame::from("id"), Frame::Integer(conn_id)),
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

#[cfg(test)]
mod tests {
    use gamestore_engine::{EngineConfig, RocksEngine};
    use tempfile::TempDir;

    use super::*;

    struct TestCtx {
        store: Arc<Store<RocksEngine>>,
        registry: CommandRegistry<RocksEngine>,
        _dir: TempDir,
    }

    impl TestCtx {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            let store =
                Arc::new(Store::open(dir.path(), &EngineConfig::default()).expect("open store"));
            TestCtx {
                store,
                registry: CommandRegistry::standard(),
                _dir: dir,
            }
        }

        fn dispatch(&self, parts: &[&str]) -> Action {
            self.dispatch_v(parts, RespVersion::V2)
        }

        fn dispatch_v(&self, parts: &[&str], version: RespVersion) -> Action {
            let args: Vec<Bytes> = parts
                .iter()
                .map(|s| Bytes::from(s.as_bytes().to_vec()))
                .collect();
            dispatch(&args, version, 7, &self.store, &self.registry)
        }
    }

    #[test]
    fn data_commands_reach_the_registry() {
        let ctx = TestCtx::new();
        assert_eq!(ctx.dispatch(&["SET", "k", "v"]).reply, Frame::ok());
        assert_eq!(
            ctx.dispatch(&["GET", "k"]).reply,
            Frame::Bulk(Bytes::from_static(b"v"))
        );
        assert_eq!(
            ctx.dispatch(&["HSET", "h", "f", "1"]).reply,
            Frame::Integer(1)
        );
        assert_eq!(
            ctx.dispatch(&["HGET", "h", "f"]).reply,
            Frame::Bulk(Bytes::from_static(b"1"))
        );
    }

    #[test]
    fn ping_is_case_insensitive_and_registry_backed() {
        let ctx = TestCtx::new();
        let action = ctx.dispatch(&["ping"]);
        assert!(matches!(action.reply, Frame::Simple(s) if s == "PONG"));
        assert!(!action.close);
    }

    #[test]
    fn unknown_command_uses_canonical_wording() {
        let ctx = TestCtx::new();
        let action = ctx.dispatch(&["NOSUCHCMD", "x"]);
        assert_eq!(
            action.reply,
            Frame::Error("ERR unknown command 'NOSUCHCMD'".to_string())
        );
    }

    #[test]
    fn quit_closes_connection() {
        let ctx = TestCtx::new();
        let action = ctx.dispatch(&["QUIT"]);
        assert!(action.close);
        assert_eq!(action.reply, Frame::ok());
    }

    #[test]
    fn flushdb_clears_the_store() {
        let ctx = TestCtx::new();
        ctx.dispatch(&["SET", "k", "v"]);
        ctx.dispatch(&["HSET", "h", "f", "1"]);
        assert_eq!(ctx.dispatch(&["DBSIZE"]).reply, Frame::Integer(2));
        assert_eq!(ctx.dispatch(&["FLUSHDB"]).reply, Frame::ok());
        assert_eq!(ctx.dispatch(&["DBSIZE"]).reply, Frame::Integer(0));
        assert_eq!(ctx.dispatch(&["GET", "k"]).reply, Frame::Null);
    }

    #[test]
    fn flushall_accepts_async_and_sync_modifiers() {
        let ctx = TestCtx::new();
        assert_eq!(ctx.dispatch(&["FLUSHALL"]).reply, Frame::ok());
        assert_eq!(ctx.dispatch(&["FLUSHALL", "ASYNC"]).reply, Frame::ok());
        assert_eq!(ctx.dispatch(&["FLUSHDB", "sync"]).reply, Frame::ok());
        assert_eq!(
            ctx.dispatch(&["FLUSHDB", "nope"]).reply,
            Frame::Error("ERR syntax error".to_string())
        );
    }

    #[test]
    fn client_housekeeping_is_tolerated() {
        let ctx = TestCtx::new();
        assert_eq!(
            ctx.dispatch(&["CLIENT", "SETINFO", "lib-name", "redis-py"])
                .reply,
            Frame::ok()
        );
        assert_eq!(
            ctx.dispatch(&["CLIENT", "SETNAME", "me"]).reply,
            Frame::ok()
        );
        assert_eq!(ctx.dispatch(&["CLIENT", "ID"]).reply, Frame::Integer(7));
        assert_eq!(
            ctx.dispatch(&["CLIENT", "GETNAME"]).reply,
            Frame::Bulk(Bytes::new())
        );
        assert!(matches!(
            ctx.dispatch(&["CLIENT", "KILL"]).reply,
            Frame::Error(_)
        ));
        assert!(matches!(ctx.dispatch(&["CLIENT"]).reply, Frame::Error(_)));
    }

    #[test]
    fn select_only_accepts_db_zero() {
        let ctx = TestCtx::new();
        assert_eq!(ctx.dispatch(&["SELECT", "0"]).reply, Frame::ok());
        assert_eq!(
            ctx.dispatch(&["SELECT", "1"]).reply,
            Frame::Error("ERR DB index is out of range".to_string())
        );
        assert_eq!(
            ctx.dispatch(&["SELECT", "abc"]).reply,
            Frame::Error("ERR value is not an integer or out of range".to_string())
        );
    }

    #[test]
    fn command_replies_empty_array() {
        let ctx = TestCtx::new();
        assert_eq!(ctx.dispatch(&["COMMAND"]).reply, Frame::Array(vec![]));
        assert_eq!(
            ctx.dispatch(&["COMMAND", "DOCS"]).reply,
            Frame::Array(vec![])
        );
    }

    #[test]
    fn hello_without_arg_keeps_version_and_replies_array_in_resp2() {
        let ctx = TestCtx::new();
        let action = ctx.dispatch(&["HELLO"]);
        assert!(action.set_version.is_none());
        assert!(matches!(action.reply, Frame::Array(_)));
    }

    #[test]
    fn hello_3_switches_to_resp3_and_replies_map() {
        let ctx = TestCtx::new();
        let action = ctx.dispatch(&["HELLO", "3"]);
        assert_eq!(action.set_version, Some(RespVersion::V3));
        assert!(matches!(action.reply, Frame::Map(_)));
    }

    #[test]
    fn hello_2_stays_resp2() {
        let ctx = TestCtx::new();
        let action = ctx.dispatch_v(&["HELLO", "2"], RespVersion::V3);
        assert_eq!(action.set_version, Some(RespVersion::V2));
        assert!(matches!(action.reply, Frame::Array(_)));
    }

    #[test]
    fn hello_bad_version_is_noproto() {
        let ctx = TestCtx::new();
        let action = ctx.dispatch(&["HELLO", "9"]);
        assert!(action.set_version.is_none());
        assert!(matches!(action.reply, Frame::Error(e) if e.starts_with("NOPROTO")));
    }

    #[test]
    fn hgetall_respects_negotiated_version() {
        let ctx = TestCtx::new();
        ctx.dispatch(&["HSET", "h", "f", "1"]);
        assert!(matches!(
            ctx.dispatch_v(&["HGETALL", "h"], RespVersion::V2).reply,
            Frame::Array(_)
        ));
        assert!(matches!(
            ctx.dispatch_v(&["HGETALL", "h"], RespVersion::V3).reply,
            Frame::Map(_)
        ));
    }
}
