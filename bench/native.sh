#!/usr/bin/env bash
# Native benchmark for the Rust `hprof` CLI (the TS bench in run.ts measures
# the old FFI path via @hprof/core, not this binary).
#
# Usage:
#   bench/native.sh              run all phases, print table, append history
#   bench/native.sh --check      additionally compare against the previous
#                                record and exit 1 on >THRESHOLD regression
#   ITER=5 WARMUP=2 THRESHOLD=1.15 bench/native.sh
#
# Measures wall time (min of ITER after WARMUP warmups) and peak RSS
# (sampled from /proc/<pid>/status) for the phases below, against the real
# files in snapshots/ when present.
set -u

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${HPROF_BIN:-$ROOT/target/release/hprof}"
RESULTS_DIR="$ROOT/bench/results"
RESULTS_FILE="$RESULTS_DIR/native.json"

ITER="${ITER:-3}"
WARMUP="${WARMUP:-1}"
THRESHOLD="${THRESHOLD:-1.20}"
CHECK=0
[ "${1:-}" = "--check" ] && CHECK=1

if [ ! -x "$BIN" ]; then
  echo "binary not found: $BIN (build with cargo build --release)" >&2
  exit 1
fi

snap() { # ext -> first matching file in snapshots/, or empty
  local f
  for f in "$ROOT"/snapshots/*."$1"; do
    [ -f "$f" ] && { echo "$f"; return; }
  done
}
HEAPSNAP="$(snap heapsnapshot)"
HEAPPROF="$(snap heapprofile)"
TIMELINE="$(snap heaptimeline)"

# peak RSS in MB for a command (polls /proc while it runs)
peak_rss_mb() {
  "$@" >/dev/null 2>&1 &
  local pid=$! peak=0 rss
  sleep 0.3
  while kill -0 "$pid" 2>/dev/null; do
    rss=$(grep VmRSS "/proc/$pid/status" 2>/dev/null | grep -o '[0-9]*')
    [ -n "$rss" ] && [ "$rss" -gt "$peak" ] && peak=$rss
    sleep 0.1
  done
  wait "$pid" 2>/dev/null
  echo $((peak / 1024))
}

# wall time in ms (one run), via date +%s%N
now_ms() { date +%s%N | awk '{print int($1/1000000)}'; }

bench_phase() { # name, cmd... — status line to stderr, JSON record to stdout
  local name="$1"; shift
  local i t0 t1 best=""
  for ((i = 0; i < WARMUP; i++)); do "$@" >/dev/null 2>&1; done
  for ((i = 0; i < ITER; i++)); do
    t0=$(now_ms); "$@" >/dev/null 2>&1; t1=$(now_ms)
    local ms=$((t1 - t0))
    { [ -z "$best" ] || [ "$ms" -lt "$best" ]; } && best=$ms
  done
  local rss
  rss=$(peak_rss_mb "$@")
  printf '  %-34s %8s ms  %6s MB\n' "$name" "$best" "$rss" >&2
  echo "{\"name\":\"$name\",\"ms\":$best,\"rss_mb\":$rss}"
}

echo "hprof native bench  bin=$BIN  iter=$ITER warmup=$WARMUP"
records=""

if [ -n "$HEAPSNAP" ]; then
  echo "heapsnapshot: $(basename "$HEAPSNAP")"
  records+="$(bench_phase analyze           "$BIN" analyze "$HEAPSNAP" --top 30)"
  records+=","
  records+="$(bench_phase analyze-retained   "$BIN" analyze "$HEAPSNAP" --top 5 --retained)"
  records+=","
  records+="$(bench_phase find               "$BIN" find "$HEAPSNAP" --name Vector3 --top 20)"
  records+=","
  records+="$(bench_phase find-exact-type    "$BIN" find "$HEAPSNAP" --name RenderingGroup --exact --type object)"
  records+=","
  records+="$(bench_phase props              "$BIN" props "$HEAPSNAP" --index 8225543)"
  records+=","
  records+="$(bench_phase inspect-id         "$BIN" inspect "$HEAPSNAP" --id 16451087)"
fi
if [ -n "$TIMELINE" ]; then
  echo "heaptimeline: $(basename "$TIMELINE")"
  [ -n "$records" ] && records+=","
  records+="$(bench_phase timeline-analyze   "$BIN" analyze "$TIMELINE" --top 30)"
fi
if [ -n "$HEAPPROF" ]; then
  echo "heapprofile: $(basename "$HEAPPROF")"
  [ -n "$records" ] && records+=","
  records+="$(bench_phase profile-analyze    "$BIN" analyze "$HEAPPROF" --top 30)"
fi

commit=$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)
ts=$(date -u +%Y-%m-%dT%H:%M:%SZ)
mkdir -p "$RESULTS_DIR"

# append to history
python3 - "$RESULTS_FILE" "$ts" "$commit" "$records" <<'EOF'
import json, sys, os
path, ts, commit, records = sys.argv[1:5]
history = []
if os.path.exists(path):
    try:
        history = json.load(open(path))
    except Exception:
        history = []
history.append({"timestamp": ts, "commit": commit, "phases": json.loads("[" + records + "]")})
json.dump(history, open(path, "w"), indent=2)
print(f"saved -> {path} (record {len(history)})")
EOF

if [ "$CHECK" -eq 1 ]; then
  python3 - "$RESULTS_FILE" "$THRESHOLD" <<'EOF'
import json, sys
path, thr = sys.argv[1], float(sys.argv[2])
history = json.load(open(path))
if len(history) < 2:
    print("no previous record to compare"); sys.exit(0)
cur, prev = history[-1], history[-2]
prev_map = {p["name"]: p for p in prev["phases"]}
fail = 0
for p in cur["phases"]:
    q = prev_map.get(p["name"])
    if not q:
        continue
    for key in ("ms", "rss_mb"):
        # tiny values are pure noise (a 3ms phase swings 50% run to run)
        floor = 10 if key == "ms" else 32
        if q[key] < floor:
            continue
        if q[key] > 0 and p[key] / q[key] > thr:
            print(f"REGRESSION {p['name']}.{key}: {p[key]} vs {q[key]} ({p[key]/q[key]:.2f}x)")
            fail = 1
        elif q[key] > 0 and p[key] / q[key] < 1 / thr:
            print(f"improved   {p['name']}.{key}: {p[key]} vs {q[key]} ({p[key]/q[key]:.2f}x)")
sys.exit(fail)
EOF
  exit $?
fi
