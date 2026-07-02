#!/usr/bin/env python3
"""End-to-end throughput baseline for a running gamestore-datanode (I-07).

Measures ops/sec through a real Redis client (redis-py) over TCP — the full
stack: client, RESP codec, dispatch, engine. Workloads model the target
pattern of 01-workload-data-model.md (small keys/values, hot Hash fields).

Modes per workload:
  - sequential: one command per round-trip (latency-bound; prints avg µs/op)
  - pipeline:   batches of --pipeline commands (throughput-bound)

Usage:
    cargo run -p gamestore-datanode --release -- --port 6390 --data-dir /tmp/bench
    python3 tests/bench_throughput.py --port 6390 [--ops 20000] [--pipeline 100]

Results are wall-clock and machine-dependent; treat them as a reproducible
baseline (same flags => comparable numbers), not absolute truth.
"""

import argparse
import time

import redis

VALUE = "x" * 64  # small value, per the workload model


def timed(fn):
    start = time.perf_counter()
    n = fn()
    elapsed = time.perf_counter() - start
    return n, elapsed


def run_sequential(r, name, op, ops):
    def body():
        for i in range(ops):
            op(r, i)
        return ops

    n, elapsed = timed(body)
    print(f"  {name:<24} sequential: {n / elapsed:>10,.0f} ops/s   "
          f"({elapsed / n * 1e6:,.1f} us/op)")


def run_pipelined(r, name, op, ops, batch):
    def body():
        done = 0
        while done < ops:
            pipe = r.pipeline(transaction=False)
            for i in range(done, min(done + batch, ops)):
                op(pipe, i)
            pipe.execute()
            done += batch
        return ops

    n, elapsed = timed(body)
    print(f"  {name:<24} pipeline({batch}): {n / elapsed:>8,.0f} ops/s")


WORKLOADS = [
    ("SET 64B", lambda c, i: c.set(f"bench:str:{i % 10000}", VALUE)),
    ("GET", lambda c, i: c.get(f"bench:str:{i % 10000}")),
    # Hot player hash: 50 fields, updates spread across them (01-workload).
    ("HSET player-field", lambda c, i: c.hset("bench:player", f"f{i % 50}", VALUE)),
    ("HGET player-field", lambda c, i: c.hget("bench:player", f"f{i % 50}")),
    ("ZADD leaderboard", lambda c, i: c.zadd("bench:lb", {f"m{i % 1000}": i})),
    ("LPUSH log", lambda c, i: c.lpush("bench:log", VALUE)),
]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--ops", type=int, default=20000)
    ap.add_argument("--pipeline", type=int, default=100)
    args = ap.parse_args()

    r = redis.Redis(host=args.host, port=args.port, socket_timeout=30)
    r.ping()
    r.flushdb()

    print(f"== throughput baseline against {args.host}:{args.port} "
          f"(ops={args.ops}, pipeline={args.pipeline}) ==")
    for name, op in WORKLOADS:
        run_sequential(r, name, op, args.ops)
    for name, op in WORKLOADS:
        run_pipelined(r, name, op, args.ops, args.pipeline)

    # Keep the bench DB from polluting a shared data dir.
    r.flushdb()


if __name__ == "__main__":
    main()
