#!/usr/bin/env bash
# Watch integration acceptance. Isolated CW_ROOT; never writes to the repo's vision/data.
# Usage: scripts/watch_acceptance.sh        (env: CW_BIN=path to skip build, CW_SKIP_BUILD=1,
#                                             INDEX_TIMEOUT=600, ACCEPT_REINDEX=1)
# Each check prints PASS / FAIL / SKIP. Exit status is non-zero if any check FAILs.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/cw-accept.XXXXXX")"
INDEX_TIMEOUT="${INDEX_TIMEOUT:-600}"
ACCEPT_REINDEX="${ACCEPT_REINDEX:-1}"
PIDS=()
PASSES=0; FAILS=0; SKIPS=0

pass() { PASSES=$((PASSES+1)); printf 'PASS  %-34s %s\n' "$1" "${2:-}"; }
fail() { FAILS=$((FAILS+1));   printf 'FAIL  %-34s %s\n' "$1" "${2:-}"; }
skip() { SKIPS=$((SKIPS+1));   printf 'SKIP  %-34s %s\n' "$1" "${2:-}"; }

cleanup() {
  for p in "${PIDS[@]:-}"; do
    [ -n "$p" ] || continue
    pkill -P "$p" 2>/dev/null || true
    kill "$p" 2>/dev/null || true
  done
  sleep 1
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill -9 "$p" 2>/dev/null || true; done
  rm -rf "$TMP"
}
trap cleanup EXIT INT TERM

free_port() { python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])'; }
jget() { python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: print(""); sys.exit(0)
for k in sys.argv[1].split("."):
    d = d.get(k) if isinstance(d, dict) else None
print("" if d is None else d)' "$1"; }
code() { curl -s -m 30 -o /dev/null -w "%{http_code}" "$@" || true; }

# ---- isolated root -------------------------------------------------------------------------
make_root() { # $1 dir, $2 modules.json source
  mkdir -p "$1/vision/data"
  cp "$2" "$1/vision/modules.json"
  cp "$REPO/vision/rules.json" "$1/vision/rules.json"
  ln -s "$REPO/vision/models" "$1/vision/models"
  ln -s "$REPO/vision/footage" "$1/vision/footage"
  echo '{"configured":true,"cameras":[]}' > "$1/vision/watch.json"
}

HEAD_PID=""
start_head() { # $1 root $2 port $3.. extra flags
  local root="$1" port="$2"; shift 2
  CW_ROOT="$root" "$BIN" head --port "$port" --name accept "$@" >>"$root/head.log" 2>&1 &
  HEAD_PID=$!; PIDS+=("$HEAD_PID")
  for _ in $(seq 1 60); do
    [ "$(code "http://127.0.0.1:$port/api/health")" = 200 ] && return 0
    kill -0 "$HEAD_PID" 2>/dev/null || return 1
    sleep 0.5
  done
  return 1
}
stop_head() {
  pkill -P "$HEAD_PID" 2>/dev/null || true
  kill "$HEAD_PID" 2>/dev/null || true
  for _ in $(seq 1 20); do kill -0 "$HEAD_PID" 2>/dev/null || break; sleep 0.25; done
  kill -9 "$HEAD_PID" 2>/dev/null || true
  sleep 1
}

# camera state: prints "completed indexed_frames error"
cam_state() { # $1 base $2 id
  curl -s -m 10 "$1/api/state" | python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: print("false 0 "); sys.exit()
for c in d.get("cameras",[]):
    if c.get("id")==sys.argv[1]:
        print(str(c.get("completed",False)).lower(), c.get("indexed_frames",0), c.get("error","") or ""); sys.exit()
print("false 0 ")' "$2"
}
wait_indexed() { # $1 base $2 id $3 timeout -> sets STATE
  local end=$((SECONDS+$3))
  while [ $SECONDS -lt $end ]; do
    STATE="$(cam_state "$1" "$2")"
    set -- "$1" "$2" "$3" $STATE
    [ "$4" = true ] && [ "${5:-0}" -gt 0 ] && return 0
    [ -n "${6:-}" ] && return 1
    sleep 2
  done
  return 1
}
legacy_count() { # $1 base
  curl -s -m 30 -X POST -H 'Content-Type: application/json' -d '{"sql":"SELECT COUNT(*) FROM sightings"}' "$1/api/query" | jget result.count
}

# ---- build ---------------------------------------------------------------------------------
if [ -n "${CW_BIN:-}" ]; then
  BIN="$CW_BIN"; skip build "CW_BIN given: $BIN"
else
  BIN="$REPO/rust/target-h/debug/cw"
  if [ "${CW_SKIP_BUILD:-0}" = 1 ]; then
    skip build "CW_SKIP_BUILD=1"
  elif CARGO_TARGET_DIR="$REPO/rust/target-h" cargo build --manifest-path "$REPO/rust/Cargo.toml" \
        --no-default-features --features watch --bin cw --offline >"$TMP/build.log" 2>&1; then
    pass build "$BIN"
  else
    fail build "cargo build failed: $(grep -m1 '^error' "$TMP/build.log" || true)"
    echo "(set CW_BIN=<prebuilt cw> to run the remaining checks against an older binary)"
    echo "SUMMARY pass=$PASSES fail=$FAILS skip=$SKIPS"; exit 1
  fi
fi
[ -x "$BIN" ] || { fail binary "not executable: $BIN"; echo "SUMMARY pass=$PASSES fail=$FAILS skip=$SKIPS"; exit 1; }
echo "binary: $BIN ($(stat -f %Sm "$BIN" 2>/dev/null || stat -c %y "$BIN"))"

# ---- main run ------------------------------------------------------------------------------
ROOT="$TMP/root"; make_root "$ROOT" "$REPO/vision/modules.json"
PORT="$(free_port)"; BASE="http://127.0.0.1:$PORT"
BEFORE_DATA="$(find "$REPO/vision/data" -type f 2>/dev/null | wc -l | tr -d ' ')"

if start_head "$ROOT" "$PORT"; then pass head_start "port $PORT, CW_ROOT=$ROOT"; else fail head_start "see $ROOT/head.log: $(tail -2 "$ROOT/head.log" | tr '\n' ' ')"; echo "SUMMARY pass=$PASSES fail=$FAILS skip=$SKIPS"; exit 1; fi

SRC='[{"id":"hall","name":"Hall","url":"vision/footage/people-detection.mp4","kind":"archive"}]'
r="$(curl -s -m 20 -X POST -H 'Content-Type: application/json' -d "$SRC" "$BASE/api/sources")"
[ "$(echo "$r" | jget ok)" = True ] && pass add_source "archive source saved via POST /api/sources" || fail add_source "$r"

bad="$(curl -s -m 20 -o /dev/null -w '%{http_code}' -X POST -d '[{"id":"x y","name":"n","url":"u","kind":"archive"}]' "$BASE/api/sources")"
[ "$bad" = 400 ] && pass reject_bad_source "invalid id -> 400" || fail reject_bad_source "got HTTP $bad"

if wait_indexed "$BASE" hall "$INDEX_TIMEOUT"; then
  pass indexing_completes "frames=$(echo $STATE | cut -d' ' -f2)"
else
  fail indexing_completes "state(completed frames error)='$STATE' within ${INDEX_TIMEOUT}s"
fi
N1="$(legacy_count "$BASE")"; N1="${N1:-}"
if [ -n "$N1" ] && [ "$N1" -gt 0 ] 2>/dev/null; then pass legacy_query "SELECT COUNT(*) FROM sightings = $N1"; else fail legacy_query "count='$N1'"; N1=0; fi

v1="$(curl -s -m 30 -o "$TMP/v1.json" -w '%{http_code}' -X POST -H 'Content-Type: application/json' \
  -d '{"class":"person","min_confidence":0.4,"scope":{}}' "$BASE/api/v1/queries")"
case "$v1" in
  404) skip v1_query "v1 not wired (POST /api/v1/queries -> 404)";;
  200) if [ "$(jget run.result_count <"$TMP/v1.json")" -gt 0 ] 2>/dev/null; then pass v1_query "run.result_count=$(jget run.result_count <"$TMP/v1.json")"; else fail v1_query "200 but no matches: $(head -c 200 "$TMP/v1.json")"; fi;;
  *)   fail v1_query "HTTP $v1: $(head -c 200 "$TMP/v1.json")";;
esac

# range request
hdr="$(curl -s -m 30 -D - -o /dev/null -H 'Range: bytes=0-1023' "$BASE/api/media/hall" | tr -d '\r')"
if echo "$hdr" | head -1 | grep -q ' 206' && echo "$hdr" | grep -qi '^content-range: bytes 0-1023/'; then
  pass media_range "206 + $(echo "$hdr" | grep -i '^content-range')"
else fail media_range "$(echo "$hdr" | head -3 | tr '\n' '|')"; fi

# idempotent and malformed batches
B='{"camera_id":"acc_cam","camera":"Acc","seq":1,"at":1.0,"ts":1700000000.5,"detections":[{"class":"person","confidence":0.9,"model":"t"}]}'
post() { curl -s -m 20 -X POST -H 'Content-Type: application/json' -d "$1" "$BASE/api/index"; }
C0="$(legacy_count "$BASE")"
r1="$(post "$B")"; C1="$(legacy_count "$BASE")"
r2="$(post "$B")"; C2="$(legacy_count "$BASE")"
if [ "$C1" = "$((C0+1))" ] && [ "$C2" = "$C1" ]; then pass idempotent_batch "count $C0 -> $C1 -> $C2 (second post ignored)"
else fail idempotent_batch "count $C0 -> $C1 -> $C2; responses: $r1 | $r2"; fi
M="$(code -X POST -H 'Content-Type: application/json' -d '{"camera_id":"x","seq":"nope"}' "$BASE/api/index")"
M2="$(code -X POST -d 'not json' "$BASE/api/index")"
[ "$M" = 400 ] && [ "$M2" = 400 ] && pass malformed_batch "400, 400" || fail malformed_batch "HTTP $M / $M2"
Cm="$(legacy_count "$BASE")"; [ "$Cm" = "$C2" ] && pass malformed_not_stored "count unchanged ($Cm)" || fail malformed_not_stored "$C2 -> $Cm"

# durability: restart without the local worker so nothing re-indexes
N_BEFORE="$(legacy_count "$BASE")"
stop_head
PIDS=()
if start_head "$ROOT" "$PORT" --no-local-worker; then
  N_AFTER="$(legacy_count "$BASE")"
  [ "$N_AFTER" = "$N_BEFORE" ] && [ "${N_AFTER:-0}" -gt 0 ] && pass durable_restart "legacy count $N_BEFORE before/after restart" || fail durable_restart "before=$N_BEFORE after=$N_AFTER"
  v1="$(code -X POST -H 'Content-Type: application/json' -d '{"class":"person","scope":{}}' "$BASE/api/v1/queries")"
  [ "$v1" = 404 ] && skip v1_durable "v1 not wired" || { [ "$v1" = 200 ] && pass v1_durable "v1 query after restart HTTP 200" || fail v1_durable "HTTP $v1"; }
else fail durable_restart "head did not restart"; fi

# restart WITH the local worker: the archive is processed again; the count must not grow
if [ "$ACCEPT_REINDEX" = 1 ]; then
  stop_head; PIDS=()
  if start_head "$ROOT" "$PORT"; then
    wait_indexed "$BASE" hall "$INDEX_TIMEOUT" || true
    N_RE="$(legacy_count "$BASE")"
    [ "$N_RE" = "$N_BEFORE" ] && pass reindex_no_duplicates "count stays $N_RE after worker re-processes the archive" \
      || fail reindex_no_duplicates "count $N_BEFORE -> $N_RE after restart + re-index (JSONL idempotency key is in memory only)"
  else fail reindex_no_duplicates "head did not restart"; fi
else skip reindex_no_duplicates "ACCEPT_REINDEX=0"; fi

# ---- auth ----------------------------------------------------------------------------------
me="$(curl -s -m 10 -o "$TMP/me.json" -w '%{http_code}' "$BASE/api/v1/auth/me")"
if [ "$me" = 404 ]; then
  skip auth_protected_routes "auth not wired (GET /api/v1/auth/me -> 404)"
elif [ "$(jget principal.anonymous <"$TMP/me.json")" = True ]; then
  skip auth_protected_routes "open mode (anonymous administrator): $(jget principal.mode <"$TMP/me.json")"
else
  a="$(code "$BASE/api/media/hall")"; b="$(code -X POST -d '[]' "$BASE/api/sources")"; c="$(code "$BASE/api/v1/audit")"
  d="$(code -X POST -d '{}' "$BASE/api/index")"; e="$(code "$BASE/api/health")"
  if [ "$a" = 401 ] && [ "$b" = 401 ] && [ "$c" = 401 ] && [ "$d" = 401 ] && [ "$e" = 200 ]; then
    pass auth_protected_routes "media/sources/audit/index -> 401 unauthenticated, health 200"
  else fail auth_protected_routes "media=$a sources=$b audit=$c index=$d health=$e"; fi
fi

# LAN exposure: an unauthenticated head must not answer on a non-loopback address
LAN="$(python3 -c 'import socket
s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)
try: s.connect(("10.255.255.255",1)); print(s.getsockname()[0])
except Exception: print("")')"
if [ -z "$LAN" ] || [ "$LAN" = 127.0.0.1 ]; then skip lan_exposure "no non-loopback address"
else
  l="$(code -m 5 "http://$LAN:$PORT/api/health")"
  if [ "$l" != 200 ]; then pass lan_exposure "not reachable on $LAN"
  elif [ "$me" != 404 ] && [ "$(jget principal.anonymous <"$TMP/me.json")" != True ]; then pass lan_exposure "reachable on $LAN but auth enforced"
  else fail lan_exposure "head reachable on $LAN:$PORT with no authentication (binds 0.0.0.0)"; fi
fi

# ---- missing model -------------------------------------------------------------------------
stop_head; PIDS=()
python3 - "$REPO/vision/modules.json" "$TMP/modules_missing.json" <<'PY'
import json,sys
m=json.load(open(sys.argv[1]))
for x in m:
    if x.get("kind")=="object": x["model"]="vision/models/does-not-exist.onnx"
json.dump(m,open(sys.argv[2],"w"))
PY
ROOT2="$TMP/root2"; make_root "$ROOT2" "$TMP/modules_missing.json"; PORT2="$(free_port)"; BASE2="http://127.0.0.1:$PORT2"
if start_head "$ROOT2" "$PORT2"; then
  curl -s -m 20 -X POST -d "$SRC" "$BASE2/api/sources" >/dev/null
  verdict=""
  for _ in $(seq 1 45); do
    S="$(cam_state "$BASE2" hall)"; set -- $S
    if [ "$1" = true ] && [ -z "${3:-}" ]; then verdict="silent-success"; break; fi
    if [ -n "${3:-}" ]; then verdict="reported"; break; fi
    sleep 2
  done
  caps="$(curl -s -m 10 "$BASE2/api/capabilities" | tr -d '\n' | head -c 300)"
  n="$(legacy_count "$BASE2")"
  case "$verdict" in
    reported) pass missing_model "camera error: $S";;
    silent-success) fail missing_model "job marked completed with no error ($S) though the model file is missing";;
    *) fail missing_model "no failure reported within 90s (state='$S', indexed rows=${n:-0}); capabilities: $caps";;
  esac
  stop_head
else fail missing_model "head with missing model did not start: $(tail -2 "$ROOT2/head.log" | tr '\n' ' ')"; fi
PIDS=()

# ---- repo hygiene --------------------------------------------------------------------------
AFTER_DATA="$(find "$REPO/vision/data" -type f 2>/dev/null | wc -l | tr -d ' ')"
[ "$BEFORE_DATA" = "$AFTER_DATA" ] && pass repo_data_untouched "vision/data file count $AFTER_DATA" || fail repo_data_untouched "vision/data files $BEFORE_DATA -> $AFTER_DATA"

echo "SUMMARY pass=$PASSES fail=$FAILS skip=$SKIPS"
[ "$FAILS" -eq 0 ]
