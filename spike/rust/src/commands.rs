//! Command dispatch: maps a parsed RESP command to a storage operation and a
//! RESP reply. Command names are case-insensitive.

use crate::resp::Reply;
use crate::storage::{now_ms, Store};

fn upper(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_ascii_uppercase()
}

fn wrong_args(cmd: &str) -> Reply {
    Reply::Error(format!("ERR wrong number of arguments for '{}' command", cmd.to_ascii_lowercase()))
}

/// Returns (reply, should_close_connection).
pub fn dispatch(store: &Store, args: &[Vec<u8>]) -> (Reply, bool) {
    if args.is_empty() {
        return (Reply::Error("ERR empty command".into()), false);
    }
    let cmd = upper(&args[0]);
    let a = &args[1..];

    let reply = match cmd.as_str() {
        "PING" => {
            if a.is_empty() {
                Reply::Simple("PONG".into())
            } else {
                Reply::Bulk(Some(a[0].clone()))
            }
        }
        "ECHO" => {
            if a.len() != 1 {
                wrong_args("echo")
            } else {
                Reply::Bulk(Some(a[0].clone()))
            }
        }
        // redis-py / redis-cli housekeeping commands: accept and no-op.
        "CLIENT" | "SELECT" | "HELLO" => Reply::ok(),
        "COMMAND" => Reply::Array(vec![]),
        "QUIT" => return (Reply::ok(), true),

        "SET" => cmd_set(store, a),
        "GET" => {
            if a.len() != 1 {
                wrong_args("get")
            } else {
                Reply::Bulk(store.get(&a[0]))
            }
        }
        "DEL" => {
            if a.is_empty() {
                wrong_args("del")
            } else {
                let n = a.iter().filter(|k| store.del(k)).count();
                Reply::Int(n as i64)
            }
        }
        "EXISTS" => {
            if a.is_empty() {
                wrong_args("exists")
            } else {
                let n = a.iter().filter(|k| store.exists(k)).count();
                Reply::Int(n as i64)
            }
        }
        "TYPE" => {
            if a.len() != 1 {
                wrong_args("type")
            } else {
                Reply::Simple(store.type_of(&a[0]).into())
            }
        }
        "EXPIRE" => cmd_expire(store, a, 1000),
        "PEXPIRE" => cmd_expire(store, a, 1),
        "TTL" => cmd_ttl(store, a, true),
        "PTTL" => cmd_ttl(store, a, false),

        "HSET" | "HMSET" => cmd_hset(store, a, &cmd),
        "HGET" => {
            if a.len() != 2 {
                wrong_args("hget")
            } else {
                Reply::Bulk(store.hget(&a[0], &a[1]))
            }
        }
        "HMGET" => {
            if a.len() < 2 {
                wrong_args("hmget")
            } else {
                let items = a[1..]
                    .iter()
                    .map(|f| Reply::Bulk(store.hget(&a[0], f)))
                    .collect();
                Reply::Array(items)
            }
        }
        "HGETALL" => {
            if a.len() != 1 {
                wrong_args("hgetall")
            } else {
                let mut items = Vec::new();
                for (f, v) in store.hgetall(&a[0]) {
                    items.push(Reply::Bulk(Some(f)));
                    items.push(Reply::Bulk(Some(v)));
                }
                Reply::Array(items)
            }
        }
        "HDEL" => {
            if a.len() < 2 {
                wrong_args("hdel")
            } else {
                Reply::Int(store.hdel(&a[0], &a[1..]))
            }
        }
        "HLEN" => {
            if a.len() != 1 {
                wrong_args("hlen")
            } else {
                Reply::Int(store.hlen(&a[0]))
            }
        }
        "HEXISTS" => {
            if a.len() != 2 {
                wrong_args("hexists")
            } else {
                Reply::Int(if store.hexists(&a[0], &a[1]) { 1 } else { 0 })
            }
        }

        // Admin / introspection (spike-only, used by the functional test).
        "FLUSHDB" | "FLUSHALL" => {
            store.flushdb();
            Reply::ok()
        }
        "DBSIZE" => Reply::Int(store.dbsize()),
        "COMPACT" => {
            store.compact();
            Reply::ok()
        }
        "RAWCOUNT" => Reply::Int(store.raw_subkey_count()),

        other => Reply::Error(format!("ERR unknown command '{}'", other.to_ascii_lowercase())),
    };
    (reply, false)
}

fn cmd_set(store: &Store, a: &[Vec<u8>]) -> Reply {
    if a.len() < 2 {
        return wrong_args("set");
    }
    let mut expire_ms = 0u64;
    let mut i = 2;
    while i < a.len() {
        match upper(&a[i]).as_str() {
            "EX" => {
                if i + 1 >= a.len() {
                    return Reply::Error("ERR syntax error".into());
                }
                match parse_u64(&a[i + 1]) {
                    Some(secs) => expire_ms = now_ms() + secs * 1000,
                    None => return Reply::Error("ERR value is not an integer or out of range".into()),
                }
                i += 2;
            }
            "PX" => {
                if i + 1 >= a.len() {
                    return Reply::Error("ERR syntax error".into());
                }
                match parse_u64(&a[i + 1]) {
                    Some(ms) => expire_ms = now_ms() + ms,
                    None => return Reply::Error("ERR value is not an integer or out of range".into()),
                }
                i += 2;
            }
            _ => return Reply::Error("ERR syntax error".into()),
        }
    }
    store.set(&a[0], &a[1], expire_ms);
    Reply::ok()
}

fn cmd_expire(store: &Store, a: &[Vec<u8>], unit_ms: u64) -> Reply {
    if a.len() != 2 {
        return wrong_args("expire");
    }
    match parse_u64(&a[1]) {
        Some(n) => Reply::Int(store.expire_ms(&a[0], now_ms() + n * unit_ms)),
        None => Reply::Error("ERR value is not an integer or out of range".into()),
    }
}

fn cmd_ttl(store: &Store, a: &[Vec<u8>], seconds: bool) -> Reply {
    if a.len() != 1 {
        return wrong_args("ttl");
    }
    let pttl = store.pttl(&a[0]);
    if pttl < 0 {
        Reply::Int(pttl)
    } else if seconds {
        Reply::Int((pttl + 999) / 1000)
    } else {
        Reply::Int(pttl)
    }
}

fn cmd_hset(store: &Store, a: &[Vec<u8>], cmd: &str) -> Reply {
    if a.len() < 3 || (a.len() - 1) % 2 != 0 {
        return wrong_args("hset");
    }
    let mut pairs = Vec::new();
    let mut i = 1;
    while i + 1 < a.len() {
        pairs.push((a[i].clone(), a[i + 1].clone()));
        i += 2;
    }
    let created = store.hset(&a[0], &pairs);
    if cmd == "HMSET" {
        Reply::ok()
    } else {
        Reply::Int(created)
    }
}

fn parse_u64(b: &[u8]) -> Option<u64> {
    std::str::from_utf8(b).ok()?.trim().parse::<u64>().ok()
}
