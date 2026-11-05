#!/usr/bin/env bash
# Run the SDK test suite on every Python 3.9 .. latest (+ free-threaded builds) with throwaway uv venvs.
#   scripts/py_matrix.sh                 all versions
#   scripts/py_matrix.sh 3.9 3.14t       just these
#   CK_VENVS=/some/dir scripts/py_matrix.sh   where the venvs live (default: a temp dir, removed at the end)
# Needs uv (https://docs.astral.sh/uv/). A version uv cannot find is reported as SKIP, not as a failure.
set -u
cd "$(dirname "$0")/.."
VERS=("$@"); [ ${#VERS[@]} -eq 0 ] && VERS=(3.9 3.10 3.11 3.12 3.13 3.13t 3.14 3.14t 3.15)
D=${CK_VENVS:-$(mktemp -d)}
rc=0; rows=()
for v in "${VERS[@]}"; do
  if [ ! -x "$D/v$v/bin/python" ] && ! uv venv -q -p "$v" "$D/v$v" 2>/dev/null; then rows+=("$v|?|SKIP (uv has no python $v)"); continue; fi
  py="$D/v$v/bin/python"
  full=$("$py" -c 'import sys;print(sys.version.split()[0] + ("" if getattr(sys,"_is_gil_enabled",lambda:True)() else " (no GIL)"))')
  out=$("$py" -W error::DeprecationWarning:clusterkit -m unittest discover -s python/tests 2>&1); s=$?
  n=$(printf '%s' "$out" | grep -E '^Ran ' | head -1)
  if [ $s -eq 0 ]; then rows+=("$v|$full|PASS  $n"); else rows+=("$v|$full|FAIL  $n"); rc=1; printf '%s\n' "$out" | tail -25 >&2; fi
done
printf '\n%-7s %-20s %s\n' version python result
for r in "${rows[@]}"; do IFS='|' read -r a b c <<<"$r"; printf '%-7s %-20s %s\n' "$a" "$b" "$c"; done
[ -z "${CK_VENVS:-}" ] && rm -rf "$D"
exit $rc
