#!/usr/bin/env bash
#
# Server-side deploy for VolX (registry model).
#
# Pulls the latest pre-built images from the registry and recreates ONLY the
# services whose image digest changed. No source checkout, no on-host build —
# the server holds just this script, the compose file, clickhouse-init.sql, and
# a `.env` with the keeper secrets.
#
#   ./deploy.sh            # pull + apply
#   ./deploy.sh --tunnel   # also (re)start the cloudflared tunnel service
#
# Env (from the `.env` beside the compose file, auto-loaded by compose):
#   SEPOLIA_RPC_URL, PRIVATE_KEY   keeper signer (required)
#   VOLX_REGISTRY (default ghcr.io/obchain), VOLX_TAG (default latest)
#   TUNNEL_TOKEN                   only with --tunnel

set -euo pipefail

cd "$(dirname "$0")"

COMPOSE_FILE="${COMPOSE_FILE:-docker-compose.prod.yml}"
DC=(docker compose -f "$COMPOSE_FILE")

PROFILE_ARGS=()
[ "${1:-}" = "--tunnel" ] && PROFILE_ARGS=(--profile tunnel)

echo "==> pulling images (${VOLX_REGISTRY:-ghcr.io/obchain}, tag ${VOLX_TAG:-latest})"
"${DC[@]}" "${PROFILE_ARGS[@]}" pull

# `up -d` diffs each service's desired vs running state and recreates only the
# ones whose image (or config) changed — unchanged services are left running.
echo "==> applying"
"${DC[@]}" "${PROFILE_ARGS[@]}" up -d --remove-orphans

# Drop any now-dangling old image layers to keep the host tidy.
echo "==> pruning dangling images"
docker image prune -f >/dev/null 2>&1 || true

echo "==> state:"
"${DC[@]}" ps
echo "==> done"
