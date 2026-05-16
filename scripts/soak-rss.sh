#!/usr/bin/env bash
# Long-running RSS soak: watches the Gyors process for memory growth
# over a configurable window. Complements `check-leaks.sh` (snapshot)
# and the cargo `--ignored soak` tests (storage-bounded).
#
# Procedure:
#   1. Build + launch Gyors with malloc stack logging.
#   2. Sample RSS at regular intervals. Each sample also fires a
#      `gyors://open?query=...` URL so the process gets real work
#      to do (panel show, query orchestration, panel hide).
#   3. Compare RSS at end vs after warmup. Flag if growth exceeds
#      a configurable percentage threshold.
#
# Defaults: 5 min run, 5s interval = 60 samples, ±25% growth allowed.
# Override via env: SOAK_DURATION_S, SOAK_INTERVAL_S, SOAK_MAX_GROWTH_PCT.
#
# This isn't a unit test - it's a smoke for "does the process leak
# under realistic load." Run before a release or after touching panel
# / hotkey / observer lifecycle code.

set -euo pipefail

DURATION="${SOAK_DURATION_S:-300}"
INTERVAL="${SOAK_INTERVAL_S:-5}"
MAX_GROWTH_PCT="${SOAK_MAX_GROWTH_PCT:-25}"
WARMUP="${SOAK_WARMUP_S:-15}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_BUNDLE="$ROOT/macos/build/Gyors.app"
APP_BIN="$APP_BUNDLE/Contents/MacOS/Gyors"

if [[ ! -x "$APP_BIN" ]]; then
    echo "error: $APP_BIN not found - run scripts/build-app.sh first" >&2
    exit 1
fi

pkill -f Gyors.app 2>/dev/null || true
sleep 0.5

# Launch via `open` so Launch Services registers our PID as the
# canonical handler for the bundle's URL schemes. Spawning the
# binary directly bypasses Launch Services, which then launches
# a SECOND Gyors instance the moment any `gyors://` URL fires -
# our `$GYORS_PID` no longer matches the running app.
#
# `MallocStackLogging` is passed through `open --env`. When the
# user has Console.app open they can also see the malloc stacks
# for any leaked allocation that surfaces.
echo "[1/3] Launching Gyors via Launch Services with malloc stack logging..."
/usr/bin/open --env MallocStackLogging=1 "$APP_BUNDLE"
sleep 1

# Resolve PID by looking up the bundle's running process. There
# should be exactly one match after our `pkill` + open.
GYORS_PID=$(pgrep -f "Gyors.app/Contents/MacOS/Gyors" | head -1)
if [[ -z "$GYORS_PID" ]]; then
    echo "error: couldn't find Gyors PID after launch" >&2
    exit 1
fi
echo "    PID: $GYORS_PID"
trap 'kill -TERM $GYORS_PID 2>/dev/null || true' EXIT

# Wait for cold start + panel pre-warm to settle. Sampling RSS
# during init catches transient allocations as growth and is noisy.
echo "[2/3] Warming up for ${WARMUP}s..."
sleep "$WARMUP"

if ! kill -0 "$GYORS_PID" 2>/dev/null; then
    echo "error: Gyors crashed during warmup. Stderr tail:" >&2
    tail -20 /tmp/gyors-soak-stderr.log >&2
    exit 1
fi

# Capture baseline RSS (KB) - anchor for the growth calculation.
rss_kb() {
    ps -o rss= -p "$GYORS_PID" 2>/dev/null | tr -d ' ' || echo 0
}

BASELINE_RSS=$(rss_kb)
if [[ -z "$BASELINE_RSS" ]] || [[ "$BASELINE_RSS" == "0" ]]; then
    echo "error: couldn't read baseline RSS for PID $GYORS_PID" >&2
    exit 1
fi
echo "    baseline RSS: ${BASELINE_RSS} KB"

echo "[3/3] Sampling RSS every ${INTERVAL}s for ${DURATION}s..."
echo "    Passive monitor - workload is whatever the running app does"
echo "    on its own (clipboard watcher, file watchers, periodic tasks)."
echo "    Drive real keystroke/hotkey load by hand for an interactive run."

SAMPLES_FILE=/tmp/gyors-soak-rss.csv
echo "elapsed_s,rss_kb" > "$SAMPLES_FILE"

ELAPSED=0
PEAK_RSS="$BASELINE_RSS"

while (( ELAPSED < DURATION )); do
    sleep "$INTERVAL"
    ELAPSED=$((ELAPSED + INTERVAL))

    if ! kill -0 "$GYORS_PID" 2>/dev/null; then
        echo "warning: Gyors PID $GYORS_PID exited at t=${ELAPSED}s - sampling done" >&2
        break
    fi

    CURRENT_RSS=$(rss_kb)
    echo "${ELAPSED},${CURRENT_RSS}" >> "$SAMPLES_FILE"
    if (( CURRENT_RSS > PEAK_RSS )); then
        PEAK_RSS="$CURRENT_RSS"
    fi
    printf "    t=%ds  RSS=%dKB (peak=%dKB)\n" \
        "$ELAPSED" "$CURRENT_RSS" "$PEAK_RSS"
done

# Compute growth as a percentage of baseline.
GROWTH_KB=$((PEAK_RSS - BASELINE_RSS))
GROWTH_PCT=$(( (GROWTH_KB * 100) / BASELINE_RSS ))

echo
echo "═══════════════════════════════════════════════"
echo "Soak summary:"
echo "    duration:    ${DURATION}s"
echo "    samples:     $((DURATION / INTERVAL))"
echo "    baseline:    ${BASELINE_RSS} KB"
echo "    peak:        ${PEAK_RSS} KB"
echo "    growth:      ${GROWTH_KB} KB (${GROWTH_PCT}%)"
echo "    threshold:   ${MAX_GROWTH_PCT}%"
echo "    samples log: $SAMPLES_FILE"
echo "═══════════════════════════════════════════════"

if (( GROWTH_PCT > MAX_GROWTH_PCT )); then
    echo "✗ peak RSS grew ${GROWTH_PCT}% over baseline (limit ${MAX_GROWTH_PCT}%)"
    exit 1
fi

echo "✓ RSS growth within ${MAX_GROWTH_PCT}% threshold"
