#!/usr/bin/env bash
# Local demo of the whole console on one machine: a root head, a standby head, two labs (A, B) of 8 simulated PCs each,
# a lab head per lab (automatic), and a few jobs (one long, one done, one failed). Opens the console; Ctrl-C stops everything.
# Safe on a laptop: power actions are dry runs, presence is off, the fleet is open (no sign-in), the long job only sleeps (no CPU).
# Usage: scripts/demo.sh [PORT]   (default 15000; uses PORT..PORT+12 and PORT+40..PORT+60 for lab heads)
set -euo pipefail
P=${1:-15000}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
B=${CK_BIN:-$ROOT/rust/target/release/ck}
D=${DEMO_DIR:-$(mktemp -d -t ck-demo)}
[ -x "$B" ] || { echo "build first: (cd rust && cargo build --release)"; exit 1; }
export CK_POWER_DRY=1 CK_PRESENCE=off CK_AUTH=off CK_FLEET=open CK_HIER_ALLOW_VIRTUAL=1
pids=()
# lab heads outlive their agent on purpose (a restarted agent reuses them), so stop them by their pid files too
stop() { for p in "${pids[@]}" $(awk 1 "$D"/pc*/labhead-*/labhead.pid 2>/dev/null); do kill "$p" 2>/dev/null || true; done; echo "demo stopped (data in $D)"; }
trap stop EXIT INT TERM

api() { python3 -c 'import sys,json,urllib.request as u
m,p,b=sys.argv[1],sys.argv[2],sys.argv[3] if len(sys.argv)>3 else None
r=u.Request("http://127.0.0.1:'$((P + 2))'/api/v1"+p,method=m,data=b.encode() if b else None,headers={"Content-Type":"application/json"})
print(u.urlopen(r).read().decode())' "$@"; }
# poll a condition every 0.2 s instead of guessing a sleep; give up after 60 s
until_ok() { local n=0; until eval "$1" > /dev/null 2>&1; do n=$((n + 1)); [ $n -lt 300 ] || { echo "demo: timed out waiting for $2 (logs in $D)"; exit 1; }; sleep 0.2; done; }
pcs() { api GET /cluster | python3 -c 'import sys,json; print(sum(1 for w in json.load(sys.stdin)["workers"] if w["node_id"]))'; }

CK_DATA=$D/root CK_PORT=$P CK_TUNE_LAB_HEAD_PORT=$((P + 40)) CK_TUNE_LAB_HEAD_MIN_PCS=4 "$B" head --no-local-worker > "$D/root.log" 2>&1 & pids+=($!)
until_ok 'api GET /cluster' "the root head"
for i in 1 2; do
  CK_DATA=$D/pc$i CK_NODE_ID=demo-pc$i CK_HEADS=127.0.0.1:$P "$B" agent --head 127.0.0.1:$P --threads 1 --virtual 8 > "$D/pc$i.log" 2>&1 & pids+=($!)
done
until_ok '[ "$(pcs)" -ge 16 ]' "16 simulated PCs"
api POST /labs '{"name":"A","w":6,"h":4}' > /dev/null
api POST /labs '{"name":"B","w":6,"h":4}' > /dev/null
python3 - "$P" <<'PY'
import json, sys, urllib.request as u
H = f"http://127.0.0.1:{int(sys.argv[1]) + 2}/api/v1"
get = lambda p: json.loads(u.urlopen(H + p).read())
def put(p, b): u.urlopen(u.Request(H + p, method="PUT", data=json.dumps(b).encode(), headers={"Content-Type": "application/json"}))
labs = {l["name"]: l["id"] for l in get("/labs")["labs"]}
on = sorted(w["node_id"] for w in get("/cluster")["workers"] if w["node_id"])
for li, pre in enumerate(sorted({n.split("/")[0] for n in on})):
    for j, n in enumerate(x for x in on if x.startswith(pre)):
        put("/nodes/" + n.replace("/", "%2F") + "/place", {"lab_id": labs["AB"[li]], "x": j % 6, "y": j // 6})
put("/hierarchy/settings", {"mode": "auto", "lab_head_min_pcs": 4})
PY
code=$(api POST /join-codes '{"uses":2,"minutes":600}' | python3 -c 'import sys,json; print(json.load(sys.stdin)["code"])')
CK_DATA=$D/standby CK_PORT=$((P + 10)) "$B" head --standby --head 127.0.0.1:$P --code "$code" --no-local-worker > "$D/standby.log" 2>&1 & pids+=($!)
api POST /jobs '{"name":"demo: sleepy list (activity, no CPU)","kind":"list","cmd":"sleep 3","items":['"$(seq -f '"%g"' -s, 1 3000 | sed 's/,$//')"'],"priority":"low"}' > /dev/null
api POST /jobs '{"name":"demo: list job","kind":"list","cmd":"sh -c \"echo $CK_ITEM | wc -c\"","items":['"$(seq -f '"%g"' -s, 1 200 | sed 's/,$//')"']}' > /dev/null
api POST /jobs '{"name":"demo: a command that fails","kind":"cmd","cmd":"false","n":20,"chunk":1}' > /dev/null
url=http://127.0.0.1:$((P + 2))/
echo "console: $url   (logs and data: $D; Ctrl-C stops the demo)"
[ -n "${NO_OPEN:-}" ] || { command -v open > /dev/null && open "$url" || true; }
wait
