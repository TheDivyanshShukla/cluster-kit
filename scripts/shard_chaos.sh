#!/usr/bin/env bash
# Chaos test for sharded search: real head + 3 worker processes on isolated roots, ports 7900-7999.
# Needs: the cw binary built with --features watch (default rust/target-d/release/cw), ffmpeg, python3, the model and footage
# folders of a checkout (CW_MAIN_CHECKOUT, default /Users/divyansh/Desktop/code/divy/cluster-kit).
# Never touches a running head on :7710 or any vision/data outside the temp dir. Writes docs/evidence/d/{chaos,latency}.md.
set -u
here="$(cd "$(dirname "$0")/.." && pwd)"
export CHAOS_DIR="${CHAOS_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/cw-chaos.XXXXXX")}"
export CW_BIN="${CW_BIN:-$here/rust/target-d/release/cw}"
[ -x "$CW_BIN" ] || { echo "build first: CARGO_TARGET_DIR=rust/target-d cargo build --release --no-default-features --features watch,coreml --bin cw" >&2; exit 2; }
# Hard timeout for the whole run (default 25 minutes).
perl -e 'alarm shift; exec @ARGV' "${CHAOS_TIMEOUT:-1500}" python3 "$here/scripts/shard_chaos.py"
rc=$?
echo "chaos exit code $rc (workdir $CHAOS_DIR)"
exit $rc
