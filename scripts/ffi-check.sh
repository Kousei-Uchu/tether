#!/usr/bin/env bash
# Builds the FFI library and the CLI, generates Python bindings, starts a real host, and runs
# crates/tether-ffi/tests/ffi_check.py against it. Linux or macOS, needs python3.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -p tether-ffi -p tether-cli --features tether-ffi/bindgen
case "$(uname)" in
  Darwin) LIB=target/debug/libtether_ffi.dylib ;;
  *) LIB=target/debug/libtether_ffi.so ;;
esac

WORK="$(mktemp -d)"
trap 'kill "${HOST_PID:-0}" 2>/dev/null || true; rm -rf "$WORK"' EXIT
cargo run -q -p tether-ffi --features bindgen --bin uniffi-bindgen -- generate --library "$LIB" --language python --out-dir "$WORK/py"
cp "$LIB" "$WORK/py/"

PORT=$((20000 + RANDOM % 20000))
CLI="$PWD/target/debug/tether"
"$CLI" --home "$WORK/host" --name ffi-host listen --bind "127.0.0.1:$PORT" --pair --no-verify --grant view,media >"$WORK/host.log" 2>&1 &
HOST_PID=$!
for _ in $(seq 1 50); do grep -q "listening" "$WORK/host.log" 2>/dev/null && break; sleep 0.1; done

export TETHER_ADDR="127.0.0.1:$PORT"
export TETHER_CODE="$(grep 'pairing code' "$WORK/host.log" | awk '{print $3}')"
export TETHER_HOST_HOME="$WORK/host" TETHER_PHONE_HOME="$WORK/phone" TETHER_CLI="$CLI"
PYTHONPATH="$WORK/py" python3 crates/tether-ffi/tests/ffi_check.py
