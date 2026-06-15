#!/usr/bin/env bash
#
# Selective production deploy for VolX.
#
# Rebuilds + restarts ONLY the services whose source changed since the last
# deploy, instead of rebuilding the whole stack every time. Intended to run
# on the always-on server from the repo root:
#
#   ./scripts/deploy.sh
#
# State is the commit SHA of the last successful deploy, stored in
# `.last-deployed` (gitignored). On the first run (no state) it does a full
# build + up and records the SHA.
#
# Path -> service mapping:
#   api/**                         -> api
#   keeper/**                      -> keeper
#   crates/engine/**               -> engine
#   crates/ingestion/**            -> ingestion
#   crates/normalizer|shared-types -> ingestion + engine (shared Rust crates)
#   Cargo.toml | Cargo.lock        -> ingestion + engine (workspace manifest)
#   docker-compose.prod.yml        -> recreate full stack (config-level change)
#   docker/clickhouse-init.sql     -> WARN only (schema loads on cold volume)
#
# Datastores (clickhouse, redis) are official images and are never rebuilt;
# they are only recreated when the compose file itself changes.

set -euo pipefail

cd "$(dirname "$0")/.."   # repo root

COMPOSE_FILE="docker/docker-compose.prod.yml"
STATE_FILE=".last-deployed"
DC=(docker compose -f "$COMPOSE_FILE")

OLD_SHA=""
[ -f "$STATE_FILE" ] && OLD_SHA="$(cat "$STATE_FILE")"

echo "==> fetching latest"
git fetch --quiet origin
BRANCH="$(git rev-parse --abbrev-ref HEAD)"
git pull --quiet --ff-only origin "$BRANCH"
NEW_SHA="$(git rev-parse HEAD)"

# First-ever deploy: no state to diff against -> bring up the whole stack.
if [ -z "$OLD_SHA" ]; then
  echo "==> no prior deploy state — full build + up"
  "${DC[@]}" up -d --build
  echo "$NEW_SHA" > "$STATE_FILE"
  echo "==> done (initial) @ $NEW_SHA"
  exit 0
fi

if [ "$OLD_SHA" = "$NEW_SHA" ]; then
  echo "==> already at $NEW_SHA — nothing to deploy"
  exit 0
fi

CHANGED="$(git diff --name-only "$OLD_SHA" "$NEW_SHA")"
echo "==> changed files ($OLD_SHA -> $NEW_SHA):"
echo "$CHANGED" | sed 's/^/    /'

match() { echo "$CHANGED" | grep -qE "$1"; }

# Compose change is config-level: recreate the full stack and stop here.
if match '^docker/docker-compose\.prod\.yml$'; then
  echo "==> compose changed — recreating full stack"
  "${DC[@]}" up -d --build
  echo "$NEW_SHA" > "$STATE_FILE"
  echo "==> done (full) @ $NEW_SHA"
  exit 0
fi

# Schema change cannot be applied by a restart (init SQL only runs on a cold
# data volume). Warn loudly; do not touch the data.
if match '^docker/clickhouse-init\.sql$'; then
  echo "!!  clickhouse-init.sql changed — schema only loads on a COLD volume."
  echo "!!  NOT applied automatically. Run a manual migration; a full reset"
  echo "!!  ('docker compose -f $COMPOSE_FILE down -v') DESTROYS all data."
fi

# Build the set of services to rebuild.
declare -A SVC
match '^api/'              && SVC[api]=1
match '^keeper/'           && SVC[keeper]=1
match '^crates/engine/'    && SVC[engine]=1
match '^crates/ingestion/' && SVC[ingestion]=1

# Shared Rust crates + the workspace manifest/lockfile affect BOTH binaries.
if match '^crates/normalizer/|^crates/shared-types/|^Cargo\.(toml|lock)$'; then
  SVC[ingestion]=1
  SVC[engine]=1
fi

SERVICES="${!SVC[*]:-}"
if [ -z "$SERVICES" ]; then
  echo "==> no service-affecting changes (docs/config/etc) — nothing to rebuild"
  echo "$NEW_SHA" > "$STATE_FILE"
  exit 0
fi

echo "==> rebuilding + restarting: $SERVICES"
# shellcheck disable=SC2086
"${DC[@]}" up -d --build $SERVICES

echo "$NEW_SHA" > "$STATE_FILE"
echo "==> done @ $NEW_SHA  (services: $SERVICES)"
