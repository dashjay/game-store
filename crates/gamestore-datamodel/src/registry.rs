//! Command dispatch: [`CommandHandler`], [`ExecCtx`] and [`CommandRegistry`]
//! (plan §2.3).
//!
//! The registry maps a **case-insensitive** command name to a handler plus a
//! Redis-style arity spec, and performs the arity check *before* invoking the
//! handler so every command gets the canonical
//! `ERR wrong number of arguments for 'xxx' command` for free. Handlers receive
//! the full argument vector (`args[0]` is the command name, exactly like
//! Redis's `argv`) so aliases such as `HSET`/`HMSET` can share one handler and
//! still reply/complain under their own name.

use std::collections::HashMap;

use bytes::Bytes;
use gamestore_engine::{EngineError, GeneralEngine, Store};
use gamestore_protocol::{Frame, RespVersion};

/// Per-command execution context (plan §2.3).
///
/// Carries everything a handler needs beyond its arguments: the [`Store`] to
/// operate on and the connection's negotiated RESP version (so replies like
/// `HGETALL` can pick map vs. flat-array encoding).
pub struct ExecCtx<'a, E: GeneralEngine> {
    /// The data store commands operate on.
    pub store: &'a Store<E>,
    /// Negotiated protocol version of the requesting connection.
    pub version: RespVersion,
}

impl<'a, E: GeneralEngine> ExecCtx<'a, E> {
    /// Context for `store` at the given protocol version.
    pub fn new(store: &'a Store<E>, version: RespVersion) -> Self {
        ExecCtx { store, version }
    }
}

/// One Redis command's execution unit (plan §2.3).
///
/// Implemented for any `Fn(&mut ExecCtx, &[Bytes]) -> Frame`, so plain
/// functions and closures register directly.
pub trait CommandHandler<E: GeneralEngine>: Send + Sync {
    /// Execute the command. `args` is the full request (`args[0]` = name);
    /// arity has already been validated against the registered spec.
    fn execute(&self, ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame;
}

impl<E, F> CommandHandler<E> for F
where
    E: GeneralEngine,
    F: Fn(&mut ExecCtx<'_, E>, &[Bytes]) -> Frame + Send + Sync,
{
    fn execute(&self, ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
        self(ctx, args)
    }
}

struct CommandSpec<E: GeneralEngine> {
    /// Lowercase name used in error messages (Redis convention).
    name_lower: String,
    /// Redis-style arity: positive = exact argc (command name included),
    /// negative = minimum argc.
    arity: i32,
    handler: Box<dyn CommandHandler<E>>,
}

/// Command registry: case-insensitive name → handler + arity check (plan §2.3).
pub struct CommandRegistry<E: GeneralEngine> {
    /// Keyed by the ASCII-uppercased command name.
    commands: HashMap<&'static str, CommandSpec<E>>,
}

impl<E: GeneralEngine + 'static> Default for CommandRegistry<E> {
    fn default() -> Self {
        CommandRegistry::new()
    }
}

impl<E: GeneralEngine + 'static> CommandRegistry<E> {
    /// An empty registry. Most callers want [`CommandRegistry::standard`].
    pub fn new() -> Self {
        CommandRegistry {
            commands: HashMap::new(),
        }
    }

    /// The full standard command set: connectivity (`PING`/`ECHO`), String +
    /// TTL, Hash (I-04), Set/ZSet/List (I-06), and the `DBSIZE`/`RAWCOUNT`/
    /// `COMPACT` introspection commands used by the consistency tests.
    pub fn standard() -> Self {
        let mut reg = CommandRegistry::new();
        crate::commands::register_all(&mut reg);
        reg
    }

    /// Register `handler` under `name` (must be ASCII-uppercase) with the given
    /// Redis-style `arity`. Replaces any previous registration of `name`.
    pub fn register(
        &mut self,
        name: &'static str,
        arity: i32,
        handler: impl CommandHandler<E> + 'static,
    ) {
        debug_assert!(
            name.chars().all(|c| c.is_ascii_uppercase() || c == '_'),
            "command names are registered uppercase: {name}"
        );
        debug_assert!(arity != 0, "arity 0 is meaningless: {name}");
        self.commands.insert(
            name,
            CommandSpec {
                name_lower: name.to_ascii_lowercase(),
                arity,
                handler: Box::new(handler),
            },
        );
    }

    /// Whether a command of this (case-insensitive) name is registered.
    pub fn contains(&self, name: &[u8]) -> bool {
        let upper = String::from_utf8_lossy(name).to_ascii_uppercase();
        self.commands.contains_key(upper.as_str())
    }

    /// Look up (case-insensitively), arity-check and execute one command,
    /// translating everything — including unknown commands and arity errors —
    /// into a RESP reply [`Frame`].
    pub fn dispatch(&self, ctx: &mut ExecCtx<'_, E>, args: &[Bytes]) -> Frame {
        let Some(first) = args.first() else {
            return Frame::error("ERR empty command");
        };
        let upper = String::from_utf8_lossy(first).to_ascii_uppercase();
        let Some(spec) = self.commands.get(upper.as_str()) else {
            return Frame::error(format!(
                "ERR unknown command '{}'",
                String::from_utf8_lossy(first)
            ));
        };
        if !arity_matches(spec.arity, args.len()) {
            return wrong_args(&spec.name_lower);
        }
        spec.handler.execute(ctx, args)
    }
}

fn arity_matches(arity: i32, argc: usize) -> bool {
    if arity > 0 {
        argc == arity as usize
    } else {
        argc >= (-arity) as usize
    }
}

// ---- shared reply helpers (used by all command modules) --------------------

/// Canonical Redis arity error for `name` (already lowercase).
pub(crate) fn wrong_args(name: &str) -> Frame {
    Frame::error(format!(
        "ERR wrong number of arguments for '{name}' command"
    ))
}

/// Canonical Redis "not an integer" error.
pub(crate) const NOT_AN_INTEGER: &str = "ERR value is not an integer or out of range";

/// Map an engine failure to its RESP error: [`EngineError::WrongType`] keeps
/// Redis's bare `WRONGTYPE ...` message; everything else is an `ERR ...`.
pub(crate) fn engine_error(e: EngineError) -> Frame {
    match e {
        EngineError::WrongType => Frame::error(e.to_string()),
        other => Frame::error(format!("ERR {other}")),
    }
}

/// Strict Redis-style integer parse (no surrounding whitespace, full i64
/// range). `None` maps to [`NOT_AN_INTEGER`].
pub(crate) fn parse_i64(b: &[u8]) -> Option<i64> {
    std::str::from_utf8(b).ok()?.parse().ok()
}

/// A bulk reply from owned bytes.
pub(crate) fn bulk(v: Vec<u8>) -> Frame {
    Frame::Bulk(Bytes::from(v))
}
