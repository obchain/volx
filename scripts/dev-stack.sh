#!/usr/bin/env bash
# Local dev stack launcher (issue #70).
#
# Sibling of `e2e-smoke.sh`: brings the same M1 pipeline online plus the
# Next.js dev server, prints a single ready URL, then blocks until the
# operator hits Ctrl-C. No assertions — this is a developer ergonomics
# script, not a pass/fail harness.
#
# Pipeline:
#
#   Deribit WS → ingestion → normalizer → ClickHouse + Redis
#                                              ↓
#                                          engine (60 s)
#                                              ↓
#                                          API REST + WS
#                                              ↓
#                                      Next.js dev server  →  browser
#
# Tears down cleanly on Ctrl-C / SIGTERM / EXIT. Volumes are preserved by
# default — re-running keeps yesterday's ClickHouse history.
#
# Requirements on PATH: docker, cargo, go, pnpm, curl.
#
# Usage: ./scripts/dev-stack.sh
#        LOGS=1 ./scripts/dev-stack.sh      # multiplex service logs to stdout
#        SKIP_BUILD=1 ./scripts/dev-stack.sh

set -euo pipefail

# ------- config --------------------------------------------------------------

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="${ROOT_DIR}/docker/docker-compose.yml"
COMPOSE_PROJECT="volx-local"

CLICKHOUSE_HOST="${CLICKHOUSE_HOST:-127.0.0.1}"
CLICKHOUSE_HTTP_PORT="${CLICKHOUSE_HTTP_PORT:-8123}"

API_HOST="${API_HOST:-localhost}"
API_PORT="${API_PORT:-8080}"
API_BASE="http://${API_HOST}:${API_PORT}"

FRONTEND_HOST="${FRONTEND_HOST:-localhost}"
FRONTEND_PORT="${FRONTEND_PORT:-3000}"
FRONTEND_URL="http://${FRONTEND_HOST}:${FRONTEND_PORT}"

LOGS="${LOGS:-0}"
SKIP_BUILD="${SKIP_BUILD:-0}"

LOG_DIR="$(mktemp -d -t volx-dev-XXXXXX)"
INGESTION_LOG="${LOG_DIR}/ingestion.log"
ENGINE_LOG="${LOG_DIR}/engine.log"
API_LOG="${LOG_DIR}/api.log"
FRONTEND_LOG="${LOG_DIR}/frontend.log"

declare -a CHILD_PIDS=()
declare -a TAIL_PIDS=()

ING_BIN="${ROOT_DIR}/target/release/volx-ingestion"
ENG_BIN="${ROOT_DIR}/target/release/volx-engine"
API_BIN="${ROOT_DIR}/api/api-bin"

# ------- helpers -------------------------------------------------------------

t_start_total=$(date +%s)
elapsed_total() { echo $(( $(date +%s) - t_start_total )); }

stage() { echo "==> $1" >&2; }

teardown() {
  local rc=$?
  set +e
  echo "" >&2
  stage "teardown"
  # Reap by binary name first — `cargo run` parents fork-exec the compiled
  # binary, and Next.js spawns child workers that don't get the SIGTERM
  # from killing the pnpm wrapper alone.
  pkill -f "target/release/volx-ingestion" 2>/dev/null
  pkill -f "target/release/volx-engine" 2>/dev/null
  pkill -f "api/api-bin" 2>/dev/null
  pkill -f "next dev" 2>/dev/null
  pkill -f "next-server" 2>/dev/null
  for pid in "${CHILD_PIDS[@]:-}"; do
    [ -n "${pid:-}" ] && kill "${pid}" 2>/dev/null
  done
  for pid in "${TAIL_PIDS[@]:-}"; do
    [ -n "${pid:-}" ] && kill "${pid}" 2>/dev/null
  done
  # Free the well-known ports in case anything escaped.
  lsof -ti ":${API_PORT}" 2>/dev/null      | xargs -r kill -9 2>/dev/null
  lsof -ti ":${FRONTEND_PORT}" 2>/dev/null | xargs -r kill -9 2>/dev/null
  # Volumes preserved on purpose — re-runs keep ClickHouse history.
  docker compose -p "${COMPOSE_PROJECT}" -f "${COMPOSE_FILE}" down >/dev/null 2>&1
  echo "logs preserved in: ${LOG_DIR}" >&2
  echo "total runtime: $(elapsed_total)s" >&2
  exit "${rc}"
}
trap teardown EXIT INT TERM

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

wait_until() {
  local label="$1"; shift
  local timeout="$1"; shift
  local deadline=$(( $(date +%s) + timeout ))
  while [ "$(date +%s)" -lt "${deadline}" ]; do
    if eval "$@" >/dev/null 2>&1; then
      return 0
    fi
    sleep 2
  done
  fail "${label} did not become ready within ${timeout}s"
}

start_service() {
  local label="$1" logfile="$2"; shift 2
  stage "spawn ${label} → ${logfile}"
  ( cd "${ROOT_DIR}" && "$@" ) >"${logfile}" 2>&1 &
  CHILD_PIDS+=("$!")
}

# Optional log multiplexer — tail every service log with a [service] prefix.
maybe_tail_logs() {
  [ "${LOGS}" = "1" ] || return 0
  stage "streaming logs to stdout (LOGS=1)"
  local files=("ingestion:${INGESTION_LOG}" "engine:${ENGINE_LOG}" "api:${API_LOG}" "frontend:${FRONTEND_LOG}")
  for entry in "${files[@]}"; do
    local name="${entry%%:*}"
    local path="${entry#*:}"
    # `tail -F` survives the file being recreated on service restart.
    ( tail -n 0 -F "${path}" 2>/dev/null | sed -u "s/^/[${name}] /" ) &
    TAIL_PIDS+=("$!")
  done
}

# ------- stages --------------------------------------------------------------

stage "compose-up (volumes preserved)"
docker compose -p "${COMPOSE_PROJECT}" -f "${COMPOSE_FILE}" up -d

stage "compose-healthy"
wait_until "clickhouse healthy" 60 \
  "curl -sS --fail http://${CLICKHOUSE_HOST}:${CLICKHOUSE_HTTP_PORT}/ping"
wait_until "redis healthy" 30 \
  "docker exec volx-redis redis-cli ping | grep -q PONG"

if [ "${SKIP_BUILD}" = "1" ]; then
  stage "build skipped (SKIP_BUILD=1)"
  [ -x "${ING_BIN}" ] || fail "SKIP_BUILD=1 but ${ING_BIN} not present"
  [ -x "${ENG_BIN}" ] || fail "SKIP_BUILD=1 but ${ENG_BIN} not present"
  [ -x "${API_BIN}" ] || fail "SKIP_BUILD=1 but ${API_BIN} not present"
else
  stage "build-rust"
  ( cd "${ROOT_DIR}" && cargo build --release -p volx-ingestion -p volx-engine ) \
    || fail "cargo build --release failed"

  stage "build-go"
  ( cd "${ROOT_DIR}/api" && go build -o "${API_BIN}" ./cmd/api ) \
    || fail "go build ./cmd/api failed"
fi

start_service "ingestion" "${INGESTION_LOG}" "${ING_BIN}"
start_service "engine"    "${ENGINE_LOG}"    "${ENG_BIN}"
start_service "api"       "${API_LOG}"       "${API_BIN}"

# Next.js inherits PORT from env; pass through whatever FRONTEND_PORT is.
stage "spawn frontend (pnpm dev) → ${FRONTEND_LOG}"
( cd "${ROOT_DIR}/frontend" && PORT="${FRONTEND_PORT}" pnpm dev ) \
  >"${FRONTEND_LOG}" 2>&1 &
CHILD_PIDS+=("$!")

stage "api-ready"
wait_until "api /v1/health" 60 "curl -sS --fail ${API_BASE}/v1/health"

stage "frontend-ready"
# Next dev needs ~2 s on warm cache, ~15 s cold (first-run TS compile).
wait_until "frontend root" 60 "curl -sS --fail ${FRONTEND_URL}"

maybe_tail_logs

# ------- ready --------------------------------------------------------------

cat <<EOF >&2

================================ READY ($(elapsed_total)s) ================================
  landing      ${FRONTEND_URL}
  chart        ${FRONTEND_URL}/chart/bvol
  api health   ${API_BASE}/v1/health
  api latest   ${API_BASE}/v1/index/bvol/latest

  logs dir     ${LOG_DIR}
  ${LOGS:+      (streaming above with [service] prefix)}

  Ctrl-C to tear down. Volumes are preserved — re-run keeps ClickHouse history.
==================================================================================

EOF

# Block until any spawned service dies. `wait -n` is bash 4+, macOS ships
# 3.2, so we poll PIDs instead. 5 s cadence is plenty — the operator
# notices a missing chart long before the script does.
while true; do
  for pid in "${CHILD_PIDS[@]}"; do
    if ! kill -0 "${pid}" 2>/dev/null; then
      fail "background service (pid ${pid}) exited unexpectedly (check ${LOG_DIR})"
    fi
  done
  sleep 5
done
