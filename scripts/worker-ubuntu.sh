#!/usr/bin/env bash
# Repair this Ubuntu PC, replace old ck instances, then show the new worker's live log.
set -euo pipefail
DIR="$HOME/Desktop/cluster-kit"
if [ ! -f "$DIR/scripts/provision.sh" ]; then
  echo "Cluster Kit provisioning script not found: $DIR/scripts/provision.sh" >&2
  exit 1
fi
cd "$DIR"
BRANCH=${CK_BRANCH:-}
if [ -z "$BRANCH" ]; then
  if command -v git >/dev/null 2>&1 && [ -d .git ]; then
    BRANCH=$(git branch --show-current)
  else
    BRANCH=main
  fi
fi
[ -n "$BRANCH" ] || { echo "Detached Git checkout: set CK_BRANCH explicitly" >&2; exit 1; }
exec sudo env CK_FLEET=open CK_AUTH=off CK_REPLACE=1 CK_FOLLOW=1 CK_ROLE=worker \
  "CK_BRANCH=$BRANCH" "CK_HEAD_IP=${CK_HEAD_IP:-}" "CK_ADMIN_KEY=${CK_ADMIN_KEY:-}" \
  bash scripts/provision.sh
