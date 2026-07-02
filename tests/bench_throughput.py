#!/usr/bin/env python3
"""End-to-end throughput baseline for a running gamestore-datanode (I-07).

Measures ops/sec through a real Redis client (redis-py) over TCP — the full
stack: client, RESP codec, dispatch, engine. Workloads model the target
pattern of 01-workload-data-model.md (small keys/values, hot Hash fields).

Modes per workload:
  - sequential: one command per round-trip (latency-bound; prints avg µs/op)
  - pipeline:   batches of --pipeline commands (throughput-bound)

Concurrent write mode (I-08): with --clients N > 1 the script also runs N
threads each on its own connection issuing SETs, and — if --metrics-url is
given — reports the WAL fsyncs issued during the run. Because the WAL coalesces
concurrent writers' syncs (group commit), writes-per-fsync climbs well above 1,
which is the "group commit reduces fsync count" evidence for the I-08 DoD.

Usage:
    cargo run -p gamestore-datanode --release -- --port 6390 --data-dir /tmp/bench
    python3 tests/bench_throughput.py --port 6390 [--ops 20000] [--pipeline 100]
    # concurrent write + fsync-coalescing measurement:
    python3 tests/bench_throughput.py --port 6390 --clients 32 \
        --metrics-url http://127.0.0.1:9600/metrics

Results are wall-clock and machine-dependent; treat them as a reproducible
baseline (same flags => comparable numbers), not absolute truth.
"""

import argparse
import threading
import time
from urllib.request import urlopen

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


def fsync_count(metrics_url):
    """Read wal_fsync_latency_seconds_count from a /metrics endpoint (or None)."""
    if not metrics_url:
        return None
    try:
        with urlopen(metrics_url, timeout=5) as resp:
            for line in resp.read().decode().splitlines():
                if line.startswith("wal_fsync_latency_seconds_count"):
                    return int(float(line.split()[-1]))
    except Exception as e:  # noqa: BLE001 - best-effort measurement
        print(f"  (could not read metrics: {e})")
    return None


def run_concurrent_writes(host, port, clients, ops, metrics_url):
    """N threads, each on its own connection, issue SETs concurrently.

    Reports aggregate ops/s and, when metrics are available, writes-per-fsync —
    the direct measure of group-commit coalescing.
    """
    per = max(1, ops // clients)
    total = per * clients
    before = fsync_count(metrics_url)

    def worker(cid):
        c = redis.Redis(host=host, port=port, socket_timeout=30)
        for i in range(per):
            c.set(f"bench:conc:{cid}:{i % 1000}", VALUE)

    threads = [threading.Thread(target=worker, args=(cid,)) for cid in range(clients)]
    start = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    elapsed = time.perf_counter() - start
    after = fsync_count(metrics_url)

    print(f"  {'SET 64B':<24} {clients} clients: {total / elapsed:>10,.0f} ops/s "
          f"({total:,} writes in {elapsed:.2f}s)")
    if before is not None and after is not None and after > before:
        fsyncs = after - before
        print(f"  {'':<24} WAL fsyncs during run: {fsyncs:,}  "
              f"=> {total / fsyncs:,.1f} writes per fsync (group commit)")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--ops", type=int, default=20000)
    ap.add_argument("--pipeline", type=int, default=100)
    ap.add_argument("--clients", type=int, default=1,
                    help="if > 1, also run a concurrent-write group-commit bench")
    ap.add_argument("--metrics-url", default=None,
                    help="/metrics URL; if set, report WAL fsyncs during the "
                         "concurrent run")
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

    if args.clients > 1:
        print(f"-- concurrent writes ({args.clients} clients) --")
        run_concurrent_writes(args.host, args.port, args.clients, args.ops,
                              args.metrics_url)

    # Keep the bench DB from polluting a shared data dir.
    r.flushdb()


if __name__ == "__main__":
    main()
