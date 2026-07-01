#!/usr/bin/env python3
"""Shared functional test for the GameStore Phase-1 spike.

Runs the *same* assertions against any server speaking our RESP2 subset, so the
Rust and C++ implementations are validated by identical checks. Uses the
standard `redis` Python client (redis-py) -- i.e. a real, unmodified Redis
client, which is the whole point of "compatible with the Redis protocol".

Usage: python3 redis_functional_test.py --host 127.0.0.1 --port 6380 [--label rust]
"""

import argparse
import sys
import time

import redis


class Checker:
    def __init__(self, label):
        self.label = label
        self.passed = 0
        self.failed = 0

    def check(self, name, got, want):
        if got == want:
            self.passed += 1
            print(f"  [PASS] {name}")
        else:
            self.failed += 1
            print(f"  [FAIL] {name}: got {got!r}, want {want!r}")

    def ok(self, name, condition):
        self.check(name, bool(condition), True)


def run(r, c: Checker):
    r.flushdb()

    # --- connectivity ---
    c.check("PING", r.ping(), True)
    c.check("ECHO", r.echo("hi"), b"hi")

    # --- String ---
    c.check("SET", r.set("k1", "v1"), True)
    c.check("GET", r.get("k1"), b"v1")
    c.check("GET missing", r.get("nope"), None)
    c.check("TYPE string", r.type("k1"), b"string")
    c.check("EXISTS", r.exists("k1"), 1)
    c.check("DEL", r.delete("k1"), 1)
    c.check("GET after DEL", r.get("k1"), None)
    c.check("EXISTS after DEL", r.exists("k1"), 0)

    # --- String overwrite + TTL ---
    r.set("t1", "x", px=150)
    c.ok("PTTL set", 0 < r.pttl("t1") <= 150)
    time.sleep(0.25)
    c.check("GET after expiry", r.get("t1"), None)
    c.check("TTL missing key", r.ttl("missing"), -2)
    r.set("t2", "y")
    c.check("TTL no expiry", r.ttl("t2"), -1)

    # --- Hash (player data, the main carrier) ---
    player = "player:{1001}"
    created = r.hset(player, mapping={"gold": "100", "level": "5", "hp": "42"})
    c.check("HSET new fields", created, 3)
    c.check("HGET", r.hget(player, "gold"), b"100")
    c.check("HLEN", r.hlen(player), 3)
    c.check("HEXISTS yes", r.hexists(player, "hp"), True)
    c.check("HEXISTS no", r.hexists(player, "mana"), False)
    c.check("HMGET", r.hmget(player, "gold", "level", "missing"),
            [b"100", b"5", None])
    c.check("HGETALL", r.hgetall(player),
            {b"gold": b"100", b"level": b"5", b"hp": b"42"})
    c.check("TYPE hash", r.type(player), b"hash")
    # update existing field -> 0 new
    c.check("HSET update existing", r.hset(player, "gold", "200"), 0)
    c.check("HGET updated", r.hget(player, "gold"), b"200")
    c.check("HDEL", r.hdel(player, "hp"), 1)
    c.check("HLEN after HDEL", r.hlen(player), 2)

    # --- Version-based subkey GC via compaction filter ---
    # Build a hash with many fields, delete it, then force a compaction and
    # assert the orphaned subkeys were physically reclaimed. Start from a clean
    # DB so RAWCOUNT reflects only this hash's subkeys.
    r.flushdb()
    big = "player:{gc}"
    mapping = {f"f{i}": str(i) for i in range(200)}
    r.hset(big, mapping=mapping)
    raw_before = int(r.execute_command("RAWCOUNT"))
    c.ok("RAWCOUNT has subkeys before DEL", raw_before >= 200)
    c.check("DEL big hash (O(1) version bump)", r.delete(big), 1)
    r.execute_command("COMPACT")
    raw_after = int(r.execute_command("RAWCOUNT"))
    c.check("subkeys reclaimed by compaction filter", raw_after, 0)

    # --- Rebuild after delete uses a fresh version (no stale leakage) ---
    r.hset(big, mapping={"a": "1", "b": "2"})
    r.delete(big)
    r.hset(big, mapping={"only": "new"})  # recreated; old version orphaned
    r.execute_command("COMPACT")
    c.check("recreated hash sees only new fields", r.hgetall(big),
            {b"only": b"new"})
    c.check("recreated hash HLEN", r.hlen(big), 1)

    r.flushdb()
    c.check("DBSIZE after flush", int(r.execute_command("DBSIZE")), 0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--label", default="server")
    args = ap.parse_args()

    print(f"== Functional test against [{args.label}] {args.host}:{args.port} ==")
    r = redis.Redis(host=args.host, port=args.port, socket_timeout=5)
    c = Checker(args.label)
    try:
        run(r, c)
    except Exception as e:  # noqa: BLE001
        print(f"  [ERROR] exception during test: {e!r}")
        c.failed += 1

    print(f"-- [{args.label}] {c.passed} passed, {c.failed} failed --")
    return 1 if c.failed else 0


if __name__ == "__main__":
    sys.exit(main())
