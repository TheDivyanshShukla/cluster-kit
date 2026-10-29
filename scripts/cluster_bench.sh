#!/usr/bin/env bash
# Start a head and N local workers (isolated data dirs), run the cluster benchmark for N = 1..K, print the table.
#
#   scripts/cluster_bench.sh [-k K] [-p PORT] [-s SEGMENT_SECS] [-t TIMEOUT_SECS] media...
#   CW=path/to/cw scripts/cluster_bench.sh -k 2 vision/footage/people-detection.mp4 vision/footage/person-bicycle-car-detection.mp4
#
# Everything runs on THIS machine, so the workers share its CPU. The numbers measure this machine; they are
# not a cluster result. Real speed-up needs workers on separate PCs (docs/CLUSTER_DEMO.md).
# The isolated head uses its own CW_ROOT (a temp dir). Your vision/data is never touched. Everything is cleaned up.
set -u
K=2; PORT=7830; SEG=""; LIMIT=900
while getopts "k:p:s:t:" o; do case $o in k) K=$OPTARG;; p) PORT=$OPTARG;; s) SEG=$OPTARG;; t) LIMIT=$OPTARG;; *) exit 2;; esac; done
shift $((OPTIND-1))
[ $# -ge 1 ] || { echo "usage: $0 [-k K] [-p PORT] [-s SEGMENT_SECS] [-t TIMEOUT_SECS] media..." >&2; exit 2; }

REPO="$(cd "$(dirname "$0")/.." && pwd)"
CW="${CW:-}"
if [ -z "$CW" ]; then for c in "$REPO"/rust/target-k/release/cw "$REPO"/rust/target/release/cw; do [ -x "$c" ] && CW="$c" && break; done; fi
[ -x "$CW" ] || { echo "cw binary not found; set CW=/path/to/cw" >&2; exit 2; }
command -v python3 >/dev/null && command -v curl >/dev/null && command -v ffmpeg >/dev/null || { echo "needs python3, curl, ffmpeg" >&2; exit 2; }

TMP="$(mktemp -d /tmp/cwbench.XXXXXX)"
PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null; done; sleep 0.5; for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill -9 "$p" 2>/dev/null; done; rm -rf "$TMP"; }
trap cleanup EXIT INT TERM

mkroot() { # dir with models and footage linked, config copied
  local d="$1"; mkdir -p "$d/vision/data"
  ln -s "$REPO/vision/models" "$d/vision/models"; ln -s "$REPO/vision/footage" "$d/vision/footage"
  cp "$REPO/vision/modules.json" "$REPO/vision/rules.json" "$d/vision/"
}
mkroot "$TMP/head"
python3 - "$REPO/vision/watch.json" "$TMP/head/vision/watch.json" <<'PY'
import json,sys
try: src=json.load(open(sys.argv[1]))
except Exception: src={}
json.dump({"configured":True,"cameras":[],"process":src.get("process",{"mode":"balanced"})},open(sys.argv[2],"w"))
PY
ln -s "$REPO/rust" "$TMP/head/rust"

H="http://127.0.0.1:$PORT"
api() { curl -s -m 15 "$@"; }
CW_ROOT="$TMP/head" CW_AUTH=off perl -e 'alarm 7200; exec @ARGV' "$CW" head --port "$PORT" --no-local-worker >"$TMP/head.log" 2>&1 &
PIDS+=($!)
for i in $(seq 1 50); do api "$H/api/health" >/dev/null && break; sleep 0.2; done
api "$H/api/health" >/dev/null || { echo "head did not start:"; tail -5 "$TMP/head.log"; exit 1; }

nodes() { api "$H/api/v1/cluster" | python3 -c 'import json,sys; print(len([n for n in json.load(sys.stdin)["nodes"] if n["state"]!="lost"]))' 2>/dev/null || echo 0; }
MEDIA=$(python3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' "$@")
for n in $(seq 1 "$K"); do
  mkroot "$TMP/w$n"
  CW_ROOT="$TMP/w$n" CW_WORKER_ID="w$n" CW_NODE_NAME="w$n" perl -e 'alarm 7200; exec @ARGV' "$CW" worker --head 127.0.0.1 --port "$PORT" >"$TMP/w$n.log" 2>&1 &
  PIDS+=($!)
  for i in $(seq 1 60); do [ "$(nodes)" -ge "$n" ] && break; sleep 0.5; done
  [ "$(nodes)" -ge "$n" ] || { echo "worker $n did not register"; tail -5 "$TMP/w$n.log"; exit 1; }
  sleep 3   # let the worker report its first stats
  BODY=$(python3 -c 'import json,sys; b={"name":"%s worker(s)"%sys.argv[1],"media":json.loads(sys.argv[2])}
if sys.argv[3]: b["segment_secs"]=int(sys.argv[3])
print(json.dumps(b))' "$n" "$MEDIA" "$SEG")
  echo ">> $n worker(s): running benchmark"
  R=$(api -X POST -H 'Content-Type: application/json' -d "$BODY" "$H/api/v1/benchmarks")
  ID=$(python3 -c 'import json,sys; print(json.loads(sys.argv[1]).get("id",""))' "$R")
  [ -n "$ID" ] || { echo "benchmark did not start: $R"; exit 1; }
  for i in $(seq 1 "$LIMIT"); do
    S=$(api "$H/api/v1/benchmarks" | python3 -c 'import json,sys; r=[x for x in json.load(sys.stdin)["runs"] if x["id"]==sys.argv[1]]; print(r[0]["state"] if r else "?")' "$ID")
    [ "$S" = running ] || break; sleep 1
  done
  echo "   $ID: $S"
done

api "$H/api/v1/benchmarks" | python3 -c '
import json,sys
runs=sorted(json.load(sys.stdin)["runs"], key=lambda r:r["started"])
print()
print("SINGLE-MACHINE RUN: workers share one machine: a gain here only means one worker was not using all of it. This is not a cluster result.")
print("%-14s %4s %9s %9s %12s %9s %10s  %s"%("run","PCs","time","x real","inferred","speed-up","efficiency","workers"))
for r in runs:
    if r["state"]!="done": print("%-14s %s %s"%(r["name"],r["state"],r.get("error") or "")); continue
    print("%-14s %4d %8.1fs %8.1fx %12d %9s %10s  %s"%(r["name"],r["worker_count"],r["wall_s"],r["speed_x"],r["frames"]["inferred"],
      ("%.2fx"%r["speedup"]) if "speedup" in r else "-", ("%d%%"%round(r["efficiency"]*100)) if "efficiency" in r else "-",
      ", ".join("%s %.0f%%"%(w["id"].split("|")[-1],100*w["share"]) for w in r["workers"])))
'
