#!/usr/bin/env bash
# End-to-end smoke for the VolX local pipeline (issue #66).
#
# Brings every M1 service online in order, waits long enough for at least
# two engine snapshots, and asserts that fresh data has propagated all the
# way to the API surface + WebSocket client. Exits non-zero with the name
# of the failed check so a regression bisects to a single hop.
#
# Pipeline:
#
#   Deribit WS → ingestion → normalizer → ClickHouse + Redis
#                                              ↓
#                                          engine (60 s)
#                                              ↓
#                                          API REST + WS
#                                              ↓
#                                       Python WS client
#
# Idempotent: tears down on exit, even on failure.
# Requirements on PATH: docker, cargo, go, curl, python3 (with `websockets`).
#
# Usage: ./scripts/e2e-smoke.sh

set -euo pipefail

# ------- config --------------------------------------------------------------

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="${ROOT_DIR}/docker/docker-compose.yml"
COMPOSE_PROJECT="volx-local"

CLICKHOUSE_DB="${CLICKHOUSE_DB:-volx}"
CLICKHOUSE_HOST="${CLICKHOUSE_HOST:-127.0.0.1}"
CLICKHOUSE_HTTP_PORT="${CLICKHOUSE_HTTP_PORT:-8123}"

API_HOST="${API_HOST:-localhost}"
API_PORT="${API_PORT:-8080}"
API_BASE="http://${API_HOST}:${API_PORT}"

# Two engine snapshots + safety margin: 60 + 60 + 15.
ENGINE_WAIT_S="${ENGINE_WAIT_S:-135}"
WS_TIMEOUT_S="${WS_TIMEOUT_S:-75}"

# Prefer the research venv (already has `websockets` from M0 work);
# fall back to system python3 otherwise. Override via env if needed.
PYTHON_BIN="${PYTHON_BIN:-}"
if [ -z "${PYTHON_BIN}" ]; then
  if [ -x "${ROOT_DIR}/research/.venv/bin/python3" ]; then
    PYTHON_BIN="${ROOT_DIR}/research/.venv/bin/python3"
  else
    PYTHON_BIN="python3"
  fi
fi

LOG_DIR="$(mktemp -d -t volx-e2e-XXXXXX)"
INGESTION_LOG="${LOG_DIR}/ingestion.log"
ENGINE_LOG="${LOG_DIR}/engine.log"
API_LOG="${LOG_DIR}/api.log"

declare -a CHILD_PIDS=()

# ------- helpers -------------------------------------------------------------

t_start_total=$(date +%s)
# Parallel indexed arrays — associative arrays would force bash 4+ and
# macOS ships bash 3.2 by default.
STAGE_NAMES=()
STAGE_START_T=()
STAGE_END_T=()

stage_begin() {
  STAGE_NAMES+=("$1")
  STAGE_START_T+=("$(date +%s)")
  STAGE_END_T+=("0")
  echo "==> $1" >&2
}
stage_end() {
  local i=$(( ${#STAGE_NAMES[@]} - 1 ))
  STAGE_END_T[${i}]=$(date +%s)
}

elapsed_total() { echo $(( $(date +%s) - t_start_total )); }

teardown() {
  local rc=$?
  set +e
  echo "==> teardown" >&2
  # `cargo run` parents fork-exec the compiled binary; killing the cargo
  # PID does not always reap the child. Hit the binaries by name first,
  # then fall back to the recorded parent PIDs.
  pkill -f "target/release/volx-ingestion" 2>/dev/null
  pkill -f "target/release/volx-normalizer" 2>/dev/null
  pkill -f "target/release/volx-engine" 2>/dev/null
  pkill -f "api/api-bin" 2>/dev/null
  pkill -f "exe/api" 2>/dev/null            # historical: go run uses tmp
  for pid in "${CHILD_PIDS[@]:-}"; do
    [ -n "${pid:-}" ] && kill "${pid}" 2>/dev/null
  done
  # Catch any stragglers still bound to the well-known ports.
  lsof -ti ":${API_PORT}" 2>/dev/null | xargs -r kill -9 2>/dev/null
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

# Polls until `cond` succeeds or `timeout` seconds elapse. `cond` is the
# command line passed in $@, evaluated as a shell expression.
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

# ClickHouse HTTP query helper. The `-G` flag is critical — without it
# curl POSTs the form-encoded query as the request body and ClickHouse
# tries to parse the literal "query=SELECT…" string as SQL.
ch_query() {
  curl -sS -G --fail \
    --data-urlencode "query=$1" \
    "http://${CLICKHOUSE_HOST}:${CLICKHOUSE_HTTP_PORT}/?database=${CLICKHOUSE_DB}"
}

start_service() {
  local label="$1" logfile="$2"; shift 2
  stage_begin "${label}"
  ( cd "${ROOT_DIR}" && "$@" ) >"${logfile}" 2>&1 &
  CHILD_PIDS+=("$!")
  stage_end "${label}"
}

# Path layout of the built binaries.
ING_BIN="${ROOT_DIR}/target/release/volx-ingestion"
ENG_BIN="${ROOT_DIR}/target/release/volx-engine"
API_BIN="${ROOT_DIR}/api/api-bin"

# ------- stages --------------------------------------------------------------

stage_begin "compose-up"
# Wipe any existing volumes so stale ticks from a previous session don't
# satisfy "fresh row" assertions before the new pipeline writes anything.
docker compose -p "${COMPOSE_PROJECT}" -f "${COMPOSE_FILE}" down --volumes >/dev/null 2>&1 || true
docker compose -p "${COMPOSE_PROJECT}" -f "${COMPOSE_FILE}" up -d
stage_end "compose-up"

stage_begin "compose-healthy"
wait_until "clickhouse healthy" 60 \
  "curl -sS --fail http://${CLICKHOUSE_HOST}:${CLICKHOUSE_HTTP_PORT}/ping"
wait_until "redis healthy" 30 \
  "docker exec volx-redis redis-cli ping | grep -q PONG"
stage_end "compose-healthy"

stage_begin "build-rust"
# `volx-normalizer` is a library that lives inside the ingestion process —
# only ingestion + engine need binaries.
( cd "${ROOT_DIR}" && cargo build --release -p volx-ingestion -p volx-engine ) \
  || fail "cargo build --release failed (see ${LOG_DIR} for prior logs)"
stage_end "build-rust"

stage_begin "build-go"
( cd "${ROOT_DIR}/api" && go build -o "${API_BIN}" ./cmd/api ) \
  || fail "go build ./cmd/api failed"
stage_end "build-go"

start_service "ingestion" "${INGESTION_LOG}" "${ING_BIN}"
start_service "engine" "${ENGINE_LOG}" "${ENG_BIN}"
start_service "api" "${API_LOG}" "${API_BIN}"

stage_begin "api-ready"
wait_until "api /v1/health" 60 "curl -sS --fail ${API_BASE}/v1/health"
stage_end "api-ready"

stage_begin "engine-cycles"
echo "    polling for ≥ 2 fresh engine snapshots (timeout ${ENGINE_WAIT_S}s)" >&2
# Wait for at least TWO snapshots so the second-cycle path (engine
# warm) is exercised before assertions run. One snapshot would race
# against the `/latest` age threshold below — by the time the four
# assertions finish, the single tick is already ~60-100 s old.
wait_until "engine ≥ 2 snapshots" "${ENGINE_WAIT_S}" \
  "[ \$(curl -sS -G --data-urlencode 'query=SELECT count(DISTINCT ts) FROM index_ticks' 'http://${CLICKHOUSE_HOST}:${CLICKHOUSE_HTTP_PORT}/?database=${CLICKHOUSE_DB}' | tr -d '[:space:]') -ge 2 ]"
stage_end "engine-cycles"

# ------- assertions ---------------------------------------------------------

stage_begin "assert-options_ticks"
opt_rows=$(ch_query "SELECT count() FROM options_ticks WHERE ts > now() - INTERVAL 1 MINUTE" | tr -d '[:space:]')
if [ -z "${opt_rows}" ] || [ "${opt_rows}" -lt 1 ]; then
  fail "options_ticks had ${opt_rows:-0} fresh rows (expected ≥ 1) in the last 60 s"
fi
echo "    options_ticks fresh rows: ${opt_rows}" >&2
stage_end "assert-options_ticks"

stage_begin "assert-index_ticks"
idx_rows=$(ch_query "SELECT count() FROM index_ticks WHERE ts > now() - INTERVAL 2 MINUTE" | tr -d '[:space:]')
if [ -z "${idx_rows}" ] || [ "${idx_rows}" -lt 1 ]; then
  fail "index_ticks had ${idx_rows:-0} fresh rows (expected ≥ 1) in the last 120 s"
fi
echo "    index_ticks fresh rows: ${idx_rows}" >&2
stage_end "assert-index_ticks"

stage_begin "assert-rest-latest"
latest_body=$(curl -sS --fail "${API_BASE}/v1/index/bvol/latest")
latest_value=$(echo "${latest_body}" | "${PYTHON_BIN}" -c "import sys,json;print(json.load(sys.stdin)['value'])")
latest_age=$(echo "${latest_body}"   | "${PYTHON_BIN}" -c "
import sys, json, datetime
j = json.load(sys.stdin)
ts = j['ts']
dt = datetime.datetime.fromisoformat(ts.replace('Z', '+00:00'))
now = datetime.datetime.now(datetime.timezone.utc)
print(int((now - dt).total_seconds()))
")
echo "    /latest bvol value=${latest_value} age=${latest_age}s" >&2
"${PYTHON_BIN}" -c "import sys; sys.exit(0 if float('${latest_value}') > 0 else 1)" \
  || fail "/latest value (${latest_value}) is not > 0"
[ "${latest_age}" -lt 90 ] || fail "/latest age (${latest_age}s) ≥ 90s"
stage_end "assert-rest-latest"

stage_begin "assert-rest-history"
hist_count=$(curl -sS --fail "${API_BASE}/v1/index/bvol/history?interval=5m&limit=12" \
  | "${PYTHON_BIN}" -c "import sys,json;print(len(json.load(sys.stdin)['bars']))")
echo "    /history bvol bars=${hist_count}" >&2
[ "${hist_count}" -ge 1 ] || fail "/history bars=${hist_count} (expected ≥ 1)"
stage_end "assert-rest-history"

stage_begin "assert-ws-stream"
"${PYTHON_BIN}" "${ROOT_DIR}/scripts/e2e-ws-client.py" \
  --url "ws://${API_HOST}:${API_PORT}/v1/stream" \
  --timeout "${WS_TIMEOUT_S}" \
  || fail "ws stream did not deliver one tick per channel inside ${WS_TIMEOUT_S}s"
stage_end "assert-ws-stream"

# ------- timing table -------------------------------------------------------

echo ""
echo "==================================== summary ===================================="
printf "%-26s %8s\n" "stage" "seconds"
printf "%-26s %8s\n" "--------------------------" "--------"
for i in "${!STAGE_NAMES[@]}"; do
  start=${STAGE_START_T[$i]}
  end=${STAGE_END_T[$i]}
  if [ "${end}" -gt 0 ]; then
    printf "%-26s %8d\n" "${STAGE_NAMES[$i]}" "$(( end - start ))"
  fi
done
printf "%-26s %8s\n" "--------------------------" "--------"
printf "%-26s %8d\n" "TOTAL" "$(elapsed_total)"
echo "================================================================================="
echo "OK"
