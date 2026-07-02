#!/usr/bin/env python3
"""End-to-end functional test for the I-06 composite types (Set/ZSet/List).

Companion to spike/test/redis_functional_test.py (which stays frozen as the
32-assertion Phase-1 baseline shared with the spikes): same style, same real
`redis` client (redis-py), run against a live gamestore-datanode over TCP.

Usage: python3 redis_composite_test.py --host 127.0.0.1 --port 6380 \
           [--label rust] [--protocol 2|3]
"""

import argparse
import sys

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

    # --- Set ---
    c.check("SADD new members", r.sadd("guild", "alice", "bob", "alice"), 2)
    c.check("SADD existing member", r.sadd("guild", "bob"), 0)
    c.check("SCARD", r.scard("guild"), 2)
    c.check("SISMEMBER yes", r.sismember("guild", "alice"), True)
    c.check("SISMEMBER no", r.sismember("guild", "carol"), False)
    c.check("SMEMBERS", set(r.smembers("guild")), {b"alice", b"bob"})
    c.check("TYPE set", r.type("guild"), b"set")
    c.check("SREM", r.srem("guild", "alice", "missing"), 1)
    c.check("SCARD after SREM", r.scard("guild"), 1)
    c.check("SREM last member deletes key", r.srem("guild", "bob"), 1)
    c.check("EXISTS after empty set", r.exists("guild"), 0)
    c.check("SMEMBERS missing key", set(r.smembers("guild")), set())

    # --- ZSet ---
    c.check("ZADD new members",
            r.zadd("lb", {"alice": 30, "bob": 10, "carol": 20}), 3)
    c.check("ZADD score update", r.zadd("lb", {"bob": 40}), 0)
    c.check("ZCARD", r.zcard("lb"), 3)
    c.check("ZSCORE", r.zscore("lb", "carol"), 20.0)
    c.check("ZSCORE missing member", r.zscore("lb", "nobody"), None)
    c.check("ZRANGE all", r.zrange("lb", 0, -1),
            [b"carol", b"alice", b"bob"])
    c.check("ZRANGE negative indexes", r.zrange("lb", -2, -1),
            [b"alice", b"bob"])
    # redis-py yields tuples under RESP2 (client-side pairing) and lists under
    # RESP3 (server sends native [member, score] pairs); normalize to tuples.
    c.check("ZRANGE WITHSCORES",
            [tuple(p) for p in r.zrange("lb", 0, 1, withscores=True)],
            [(b"carol", 20.0), (b"alice", 30.0)])
    c.check("ZRANGEBYSCORE inclusive",
            r.zrangebyscore("lb", 20, 30), [b"carol", b"alice"])
    c.check("ZRANGEBYSCORE exclusive/inf",
            r.zrangebyscore("lb", "(20", "+inf"), [b"alice", b"bob"])
    c.check("ZRANGEBYSCORE LIMIT",
            r.zrangebyscore("lb", "-inf", "+inf", start=1, num=1), [b"alice"])
    c.check("TYPE zset", r.type("lb"), b"zset")
    c.check("ZREM", r.zrem("lb", "alice", "nobody"), 1)
    c.check("ZCARD after ZREM", r.zcard("lb"), 2)

    # --- List ---
    c.check("RPUSH", r.rpush("log", "e1", "e2"), 2)
    c.check("LPUSH", r.lpush("log", "e0"), 3)
    c.check("LLEN", r.llen("log"), 3)
    c.check("LRANGE all", r.lrange("log", 0, -1), [b"e0", b"e1", b"e2"])
    c.check("LRANGE clamps", r.lrange("log", -100, 100),
            [b"e0", b"e1", b"e2"])
    c.check("TYPE list", r.type("log"), b"list")
    c.check("LPOP", r.lpop("log"), b"e0")
    c.check("RPOP count", r.rpop("log", 2), [b"e2", b"e1"])
    c.check("LPOP emptied key", r.lpop("log"), None)
    c.check("EXISTS after drained list", r.exists("log"), 0)

    # --- WRONGTYPE across families ---
    r.set("str", "v")
    for name, fn in [
        ("SADD on string", lambda: r.sadd("str", "m")),
        ("ZADD on string", lambda: r.zadd("str", {"m": 1})),
        ("LPUSH on string", lambda: r.lpush("str", "v")),
    ]:
        try:
            fn()
            c.ok(f"{name} raises WRONGTYPE", False)
        except redis.ResponseError as e:
            c.ok(f"{name} raises WRONGTYPE", "WRONGTYPE" in str(e))

    # --- TTL / DEL on composite types ---
    r.sadd("tmp", "m")
    c.check("EXPIRE on set", r.expire("tmp", 100), True)
    c.ok("TTL on set", 0 < r.ttl("tmp") <= 100)
    c.check("DEL set", r.delete("tmp"), 1)

    # --- Version-based GC covers the new record families ---
    r.flushdb()
    r.sadd("gs", *[f"m{i}" for i in range(100)])
    r.zadd("gz", {f"m{i}": i for i in range(100)})
    r.rpush("gl", *[f"v{i}" for i in range(100)])
    # set 100 + zset 100*2 (member + score index) + list 100.
    c.check("RAWCOUNT before DEL", int(r.execute_command("RAWCOUNT")), 400)
    c.check("DEL composite keys", r.delete("gs", "gz", "gl"), 3)
    r.execute_command("COMPACT")
    c.check("records reclaimed by compaction filter",
            int(r.execute_command("RAWCOUNT")), 0)

    r.flushdb()
    c.check("DBSIZE after flush", int(r.execute_command("DBSIZE")), 0)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--label", default="server")
    ap.add_argument("--protocol", type=int, default=2, choices=(2, 3))
    args = ap.parse_args()

    print(f"== Composite-type test against [{args.label}] "
          f"{args.host}:{args.port} (RESP{args.protocol}) ==")
    r = redis.Redis(host=args.host, port=args.port, socket_timeout=5,
                    protocol=args.protocol)
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
