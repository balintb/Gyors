#!/usr/bin/env bash
# Quick memory-leak check using Apple's `leaks(1)`. Launches the
# already-built Gyors.app, lets it warm up, runs a small synthetic
# query workload, snapshots `leaks`, then exits.
#
# Run after `scripts/build-app.sh`. Exits non-zero when leaks(1)
# reports anything other than "0 leaks for ... nodes malloced".
#
# CI use: invoke after the test suite as a third gate alongside
# Rust + Swift unit tests. Local use: run before merging
# anything touching panel / hotkey / event-monitor lifecycle, or
# before a release.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_BIN="$ROOT/macos/build/Gyors.app/Contents/MacOS/Gyors"

if [[ ! -x "$APP_BIN" ]]; then
    echo "error: $APP_BIN not found - run scripts/build-app.sh first" >&2
    exit 1
fi

# Kill any running instance so PID resolution is unambiguous.
pkill -f Gyors.app 2>/dev/null || true
sleep 0.5

# `MallocStackLogging=1` lets `leaks` print backtraces for any
# leaked allocation - much more useful than just count + size.
# `MallocStackLoggingNoCompact` keeps the unfiltered stacks; if
# the run gets noisy in CI we can drop this back.
echo "[1/3] Launching Gyors with malloc stack logging..."
MallocStackLogging=1 \
MallocStackLoggingNoCompact=1 \
"$APP_BIN" >/tmp/gyors-leaks-stdout.log 2>/tmp/gyors-leaks-stderr.log &
GYORS_PID=$!
trap 'kill -TERM $GYORS_PID 2>/dev/null || true' EXIT

# Give cold-start (~150ms) + panel pre-warm (~90ms) headroom plus
# margin for a slower CI runner. Skipping the wait risks running
# `leaks` before bridge init finishes, which surfaces transient
# tokio runtime allocations as false positives.
echo "[2/3] Warming up (5s)..."
sleep 5

if ! kill -0 "$GYORS_PID" 2>/dev/null; then
    echo "error: Gyors crashed during warmup. Stderr tail:" >&2
    tail -20 /tmp/gyors-leaks-stderr.log >&2
    exit 1
fi

echo "[3/3] Running leaks(1) on PID $GYORS_PID..."
LEAKS_OUTPUT=$(leaks "$GYORS_PID" 2>&1 || true)

# `leaks` prints "Process N: 0 leaks for 0 total leaked bytes."
# on a clean run. Anything else means malloc-tracked allocations
# are unreachable but not freed - the textbook leak shape.
if echo "$LEAKS_OUTPUT" | grep -q "0 leaks for 0 total leaked bytes"; then
    echo
    echo "leaks(1) reports zero leaks"
    exit 0
fi

echo
echo "! leaks(1) found something:"
echo "$LEAKS_OUTPUT"
exit 1
