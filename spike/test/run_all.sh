#!/usr/bin/env bash
# Build both spikes, start each server, run the shared functional test against
# both, and (optionally) a redis-cli smoke test. Exits non-zero if either fails.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SPIKE_DIR="$(dirname "$SCRIPT_DIR")"
RUST_DIR="$SPIKE_DIR/rust"
CPP_DIR="$SPIKE_DIR/cpp"

RUST_PORT=6390
CPP_PORT=6391
RUST_DB="$(mktemp -d)/rust"
CPP_DB="$(mktemp -d)/cpp"

RUST_BIN="$RUST_DIR/target/release/gamestore-spike"
CPP_BIN="$CPP_DIR/build/gamestore_spike"

PIDS=()
cleanup() {
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" 2>/dev/null || true
  done
}
trap cleanup EXIT

echo "==================== BUILD ===================="
echo "[build] Rust spike..."
( cd "$RUST_DIR" && cargo build --release )
echo "[build] C++ spike..."
( cd "$CPP_DIR" && CXX=g++ CC=gcc cmake -S . -B build -DCMAKE_BUILD_TYPE=Release -DCMAKE_CXX_COMPILER=g++ >/dev/null && cmake --build build -j"$(nproc)" >/dev/null )

wait_for_port() {
  local port="$1"
  for _ in $(seq 1 50); do
    if redis-cli -p "$port" ping >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  echo "server on port $port did not come up" >&2
  return 1
}

echo "==================== RUST ====================="
"$RUST_BIN" --port "$RUST_PORT" --db "$RUST_DB" &
PIDS+=($!)
wait_for_port "$RUST_PORT"
RUST_RC=0
python3 "$SCRIPT_DIR/redis_functional_test.py" --port "$RUST_PORT" --label rust || RUST_RC=$?

echo "==================== C++ ======================"
"$CPP_BIN" --port "$CPP_PORT" --db "$CPP_DB" &
PIDS+=($!)
wait_for_port "$CPP_PORT"
CPP_RC=0
python3 "$SCRIPT_DIR/redis_functional_test.py" --port "$CPP_PORT" --label cpp || CPP_RC=$?

echo "==================== redis-cli smoke =========="
echo "[rust] $(redis-cli -p "$RUST_PORT" set smoke hello) / $(redis-cli -p "$RUST_PORT" get smoke)"
echo "[cpp]  $(redis-cli -p "$CPP_PORT" set smoke hello) / $(redis-cli -p "$CPP_PORT" get smoke)"

echo "==================== RESULT ==================="
if [[ "$RUST_RC" -eq 0 && "$CPP_RC" -eq 0 ]]; then
  echo "ALL TESTS PASSED (rust + cpp)"
  exit 0
else
  echo "FAILURES: rust_rc=$RUST_RC cpp_rc=$CPP_RC"
  exit 1
fi
