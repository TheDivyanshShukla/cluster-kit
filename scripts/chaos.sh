#!/usr/bin/env bash
# HA-09 chaos script (first version): starts a head + agents on this machine, breaks them mid-job,
# checks the job still ends with the exact result. PASS/FAIL per scenario; exit 1 on any FAIL.
# Env: CK_PORT (default 7830), CK_BIN (default: build rust/target/release/ck), CHAOS_N (SHA range, default 1e8).
set -euo pipefail
cd "$(dirname "$0")/.."
export CK_PORT="${CK_PORT:-7830}"
N="${CHAOS_N:-100000000}"
LOG="${CHAOS_LOG:-$(mktemp -d)}"
export CK_DATA="${CK_DATA:-$LOG/data}"   # never touch ./ck-data
export CK_FLEET="${CK_FLEET:-open}"      # chaos tests run unregistered virtual agents on one machine
export CK_PRESENCE="${CK_PRESENCE:-off}"   # a busy dev machine would otherwise mark every agent "in use"
if [ -z "${CK_BIN:-}" ]; then
  (cd rust && cargo build --release --no-default-features) >&2
  CK_BIN="$PWD/rust/target/release/ck"
fi
PIDS=()
TO=$(command -v timeout || command -v gtimeout || true) # macOS has neither by default: run unguarded
limit() { local t=$1; shift; if [ -n "$TO" ]; then "$TO" "$t" "$@"; else "$@"; fi; }
cleanup() { for p in ${PIDS[@]+"${PIDS[@]}"}; do kill -CONT "$p" 2>/dev/null || true; kill "$p" 2>/dev/null || true; done; wait 2>/dev/null || true; }
trap cleanup EXIT
FAILS=0
report() { if [ "$2" = ok ]; then echo "PASS $1"; else echo "FAIL $1 ($2)"; FAILS=$((FAILS + 1)); fi; }

start_head() { "$CK_BIN" head --no-local-worker >"$LOG/head.log" 2>&1 & PIDS+=($!); sleep 1; }
start_agent() { "$CK_BIN" agent --head 127.0.0.1 --threads 2 "$@" >"$LOG/agent$RANDOM.log" 2>&1 & LAST=$!; PIDS+=($LAST); }
stop_all() { cleanup; PIDS=(); sleep 1; }
# hit count line: "<k> hashes start with ..."
sha_job() { limit "${JOB_TIMEOUT:-120}" "$CK_BIN" run --sha 0000 --n "$N" --chunk 1000000 2>/dev/null | awk '/hashes start with/ {print $1}'; }

# (a) baseline
start_head; for _ in 1 2 3; do start_agent; done; sleep 2
BASE=$(sha_job || true)
if [ -n "$BASE" ] && [ "$BASE" -gt 0 ]; then report "a baseline hit count ($BASE)" ok; else report "a baseline" "no count"; fi

# (b)/(c) break one of 3 agents mid-job
mid_job() { # $1 = signal args, runs the job while killing the 3rd agent after 1 s
  local out="$LOG/job.out"
  ( sha_job >"$out" ) & local jp=$!
  sleep 1; kill "$1" "$VICTIM" 2>/dev/null || true
  wait "$jp" || true; cat "$out"
}
for sc in b c; do
  stop_all; start_head; start_agent; start_agent; start_agent; VICTIM=$LAST; sleep 2
  if [ $sc = b ]; then SIG=-STOP; else SIG=-9; fi
  GOT=$(mid_job $SIG || true)
  kill -CONT "$VICTIM" 2>/dev/null || true
  if [ -n "$BASE" ] && [ "$GOT" = "$BASE" ]; then report "$sc kill $SIG one agent mid-job" ok; else report "$sc kill $SIG one agent mid-job" "got '${GOT:-none}' want '$BASE'"; fi
done

# (d) 20 virtual agents, synthetic job, exact count
stop_all; start_head; start_agent --virtual 20; sleep 2
OUT=$(limit 60 "$CK_BIN" run --synth 1000 --n 2000000 --chunk 10000 2>&1 || true)
if echo "$OUT" | grep -q '^2000000 numbers counted'; then report "d synth with --virtual 20" ok; else report "d synth with --virtual 20" "$(echo "$OUT" | tail -1)"; fi

# (e) head kill with a standby: same CK_PORT (a standby listens on nothing until it promotes), own CK_DATA
stop_all; export CK_DATA="$LOG/data-e"; start_head; HEAD=$!   # fresh data: d's 20 virtual nodes would count in membership
CK_DATA="$LOG/data-standby" "$CK_BIN" head --standby --head 127.0.0.1 --no-local-worker >"$LOG/standby.log" 2>&1 & PIDS+=($!)
for _ in 1 2 3; do start_agent; done; sleep 3
T0=$(date +%s)
( CK_FAILOVER_SECS=60 sha_job >"$LOG/job-e.out" ) & JP=$!
sleep 1; kill -9 "$HEAD" 2>/dev/null || true
wait "$JP" || true; GOT=$(cat "$LOG/job-e.out")
if [ -n "$BASE" ] && [ "$GOT" = "$BASE" ]; then report "e kill -9 head with standby ($(( $(date +%s) - T0 )) s)" ok; else report "e kill -9 head with standby" "got '${GOT:-none}' want '$BASE'"; fi

echo "logs: $LOG"
[ "$FAILS" = 0 ]
