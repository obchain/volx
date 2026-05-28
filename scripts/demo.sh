#!/usr/bin/env bash
# VolX demo runner. Idempotent — safe to re-run.
#
# What it does:
#   1. Verifies Docker daemon is up.
#   2. Picks a free api host port in [8090, 8099]; if 8090 is held
#      by something that is NOT a VolX container, falls back.
#   3. Tears down any prior VolX compose stack to start clean.
#   4. Kills stale `next dev` processes from previous runs.
#   5. Brings the compose stack up with the chosen api port.
#   6. Polls /v1/health until "ok" (max 90 s).
#   7. Writes frontend/.env.local with the chosen port.
#   8. Wipes frontend/.next so next dev rebuilds against the new env.
#   9. Starts `npm run dev` in the background.
#  10. Waits for the frontend to respond (max 60 s).
#  11. Starts `caffeinate -di` in the background to keep the laptop
#      awake (Mac only — silently skipped elsewhere).
#  12. Opens the landing page in the browser.
#  13. Prints status + teardown instructions and exits.
#
# Usage:
#   scripts/demo.sh           # bring everything up
#   scripts/demo.sh --down    # tear everything down
#   scripts/demo.sh --status  # show what's running
#
# Logs:
#   /tmp/volx-frontend.log    — Next dev server output
#   /tmp/volx-caffeinate.pid  — caffeinate PID for tear-down
#   /tmp/volx-frontend.pid    — npm PID for tear-down

set -euo pipefail

# ─── Resolve repo paths ───────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
COMPOSE_FILE="$REPO_ROOT/docker/docker-compose.yml"
FRONTEND_DIR="$REPO_ROOT/frontend"
ENV_LOCAL="$FRONTEND_DIR/.env.local"

FRONTEND_LOG="/tmp/volx-frontend.log"
FRONTEND_PID="/tmp/volx-frontend.pid"
CAFFEINATE_PID="/tmp/volx-caffeinate.pid"

# ─── Colours (no-op when not a TTY) ───────────────────────────────────
if [[ -t 1 ]]; then
  C_RESET=$'\033[0m'
  C_BOLD=$'\033[1m'
  C_DIM=$'\033[2m'
  C_CYAN=$'\033[36m'
  C_GREEN=$'\033[32m'
  C_YELLOW=$'\033[33m'
  C_RED=$'\033[31m'
else
  C_RESET=""; C_BOLD=""; C_DIM=""; C_CYAN=""; C_GREEN=""; C_YELLOW=""; C_RED=""
fi

log()  { echo "${C_DIM}[$(date +%H:%M:%S)]${C_RESET} $*"; }
ok()   { echo "${C_GREEN}✓${C_RESET} $*"; }
warn() { echo "${C_YELLOW}!${C_RESET} $*"; }
err()  { echo "${C_RED}✗${C_RESET} $*" >&2; }
hdr()  { echo; echo "${C_BOLD}${C_CYAN}── $* ──${C_RESET}"; }

# ─── Tear-down path ───────────────────────────────────────────────────
do_down() {
  hdr "tearing down"

  if [[ -f "$FRONTEND_PID" ]] && kill -0 "$(cat "$FRONTEND_PID")" 2>/dev/null; then
    log "stopping next dev (pid $(cat "$FRONTEND_PID"))"
    kill "$(cat "$FRONTEND_PID")" 2>/dev/null || true
    rm -f "$FRONTEND_PID"
  fi
  # Fallback — kill any leftover next dev anywhere
  pkill -f "next dev" 2>/dev/null || true
  ok "next dev stopped"

  if [[ -f "$CAFFEINATE_PID" ]] && kill -0 "$(cat "$CAFFEINATE_PID")" 2>/dev/null; then
    log "stopping caffeinate (pid $(cat "$CAFFEINATE_PID"))"
    kill "$(cat "$CAFFEINATE_PID")" 2>/dev/null || true
    rm -f "$CAFFEINATE_PID"
  fi
  ok "caffeinate stopped"

  log "compose down (timeout 5s)"
  docker compose -f "$COMPOSE_FILE" down -t 5 2>&1 | sed 's/^/    /' || true
  ok "stack down"
  echo
}

# ─── Status path ──────────────────────────────────────────────────────
do_status() {
  hdr "volx status"
  docker compose -f "$COMPOSE_FILE" ps 2>/dev/null || warn "compose not running"
  echo
  if [[ -f "$FRONTEND_PID" ]] && kill -0 "$(cat "$FRONTEND_PID")" 2>/dev/null; then
    ok "next dev running (pid $(cat "$FRONTEND_PID"))"
  else
    warn "next dev not tracked"
  fi
  if [[ -f "$CAFFEINATE_PID" ]] && kill -0 "$(cat "$CAFFEINATE_PID")" 2>/dev/null; then
    ok "caffeinate running (pid $(cat "$CAFFEINATE_PID"))"
  else
    warn "caffeinate not running"
  fi
  echo
}

# ─── Subcommand dispatch ──────────────────────────────────────────────
case "${1:-up}" in
  --down|down)
    do_down
    exit 0
    ;;
  --status|status)
    do_status
    exit 0
    ;;
  --help|-h|help)
    sed -n '2,30p' "$0"
    exit 0
    ;;
  --up|up|"")
    : # fall through to bring-up
    ;;
  *)
    err "unknown arg: $1"
    sed -n '2,30p' "$0" >&2
    exit 2
    ;;
esac

# ═════════════════════════════════════════════════════════════════════
#                            BRING-UP
# ═════════════════════════════════════════════════════════════════════

hdr "volx demo runner"
log "repo:        $REPO_ROOT"
log "compose:     $COMPOSE_FILE"
log "frontend:    $FRONTEND_DIR"

# ─── 1. Docker daemon ─────────────────────────────────────────────────
hdr "checking docker"
if ! docker info > /dev/null 2>&1; then
  err "docker daemon not reachable. Start Docker Desktop and re-run."
  exit 1
fi
ok "docker is up"

# ─── 2. Pick a free api host port ─────────────────────────────────────
hdr "selecting api host port"

# Returns 0 if the given TCP port is free on 127.0.0.1, non-zero otherwise.
port_is_free() {
  ! lsof -nP -i "TCP@127.0.0.1:$1" -sTCP:LISTEN 2>/dev/null | grep -q LISTEN
}

# Returns 0 if the listener on the port is a docker container (so it's
# probably our own previous run and we should reuse the port after a
# fresh `docker compose up`).
listener_is_docker() {
  local p=$1
  lsof -nP -i "TCP@127.0.0.1:$p" -sTCP:LISTEN 2>/dev/null | awk 'NR>1 {print $1}' | grep -qi "docker\|com.docker"
}

API_PORT=""
for p in 8090 8091 8092 8093 8094 8095 8096 8097 8098 8099; do
  if port_is_free "$p"; then
    API_PORT="$p"
    break
  fi
  if listener_is_docker "$p"; then
    # Docker holds it — almost certainly our previous run. After the
    # `compose down` step below the port frees up. Trust it.
    API_PORT="$p"
    break
  fi
  warn "port $p is held by something non-docker — trying next"
done

if [[ -z "$API_PORT" ]]; then
  err "no free port found in 8090-8099. Free one up and re-run."
  exit 1
fi

if [[ "$API_PORT" != "8090" ]]; then
  warn "relocated api host port: 8090 → $API_PORT"
else
  ok "api host port: 8090"
fi

export VOLX_API_HOST_PORT="$API_PORT"
TUNNEL_BASE="http://127.0.0.1:${API_PORT}"

# ─── 3. Tear down any prior run ───────────────────────────────────────
hdr "cleaning prior state"

# Kill any tracked frontend PID
if [[ -f "$FRONTEND_PID" ]]; then
  if kill -0 "$(cat "$FRONTEND_PID")" 2>/dev/null; then
    log "killing tracked next dev (pid $(cat "$FRONTEND_PID"))"
    kill "$(cat "$FRONTEND_PID")" 2>/dev/null || true
  fi
  rm -f "$FRONTEND_PID"
fi

# Catch-all for stale next dev processes from previous sessions
if pgrep -f "next dev" > /dev/null 2>&1; then
  log "killing stale next dev processes"
  pkill -f "next dev" 2>/dev/null || true
  sleep 1
fi
ok "no stale next dev"

# Down + remove orphans to handle stack-on-port-change situations
log "compose down (timeout 5s, no volume wipe)"
docker compose -f "$COMPOSE_FILE" down -t 5 --remove-orphans 2>&1 | sed 's/^/    /' || true
ok "prior compose down"

# ─── 4. Bring compose up ──────────────────────────────────────────────
hdr "starting backend"
log "compose up -d (api port = $API_PORT)"
docker compose -f "$COMPOSE_FILE" up -d 2>&1 | sed 's/^/    /'
ok "containers created"

# ─── 5. Wait for /v1/health ───────────────────────────────────────────
log "waiting for /v1/health (max 90s)"
DEADLINE=$(($(date +%s) + 90))
HEALTH_OK=0
while (( $(date +%s) < DEADLINE )); do
  if curl -sS --max-time 2 "${TUNNEL_BASE}/v1/health" 2>/dev/null | grep -q '"status":"ok"'; then
    HEALTH_OK=1
    break
  fi
  sleep 2
done

if [[ "$HEALTH_OK" -ne 1 ]]; then
  warn "api health did not reach ok within 90s — engine may still be warming up"
  warn "current health: $(curl -sS --max-time 2 "${TUNNEL_BASE}/v1/health" 2>/dev/null || echo 'unreachable')"
else
  ok "api healthy on ${TUNNEL_BASE}"
fi

# ─── 6. Wire frontend to chosen port ──────────────────────────────────
hdr "wiring frontend"
cat > "$ENV_LOCAL" <<EOF
# Generated by scripts/demo.sh — do not edit by hand. Tracks the
# host port the api was actually bound to on the most recent run.
API_PROXY_TARGET=${TUNNEL_BASE}
NEXT_PUBLIC_API_BASE=${TUNNEL_BASE}
EOF
ok "wrote $ENV_LOCAL (api → ${TUNNEL_BASE})"

# Wipe Next cache so the env change actually takes effect.
log "wiping frontend/.next cache"
rm -rf "$FRONTEND_DIR/.next" 2>/dev/null || true
ok "cache cleared"

# ─── 7. Start npm run dev in background ───────────────────────────────
hdr "starting frontend"
cd "$FRONTEND_DIR"

if [[ ! -d node_modules ]]; then
  log "node_modules missing — running npm install (first run)"
  npm install --no-fund --no-audit 2>&1 | tail -3
fi

log "spawning next dev (logs → $FRONTEND_LOG)"
nohup npm run dev > "$FRONTEND_LOG" 2>&1 &
echo $! > "$FRONTEND_PID"
disown
ok "next dev pid $(cat "$FRONTEND_PID")"

# Discover the chosen port (Next auto-bumps when 3000 is held)
log "waiting for next dev to bind (max 60s)"
FRONTEND_PORT=""
FRONTEND_URL=""
DEADLINE=$(($(date +%s) + 60))
while (( $(date +%s) < DEADLINE )); do
  if [[ -f "$FRONTEND_LOG" ]]; then
    FRONTEND_PORT=$(grep -oE "Local:[[:space:]]+http://localhost:[0-9]+" "$FRONTEND_LOG" \
      | head -1 | grep -oE "[0-9]+$" || true)
    if [[ -n "$FRONTEND_PORT" ]] && curl -sS --max-time 2 -o /dev/null \
        "http://localhost:${FRONTEND_PORT}/" 2>/dev/null; then
      FRONTEND_URL="http://localhost:${FRONTEND_PORT}"
      break
    fi
  fi
  sleep 1
done

if [[ -z "$FRONTEND_URL" ]]; then
  err "next dev did not become reachable in 60s"
  tail -20 "$FRONTEND_LOG" | sed 's/^/    /'
  exit 1
fi
ok "frontend live on $FRONTEND_URL"

# ─── 8. Caffeinate (Mac only) ─────────────────────────────────────────
hdr "keeping system awake"
if [[ "$(uname)" == "Darwin" ]] && command -v caffeinate > /dev/null; then
  # Only spawn one
  if [[ -f "$CAFFEINATE_PID" ]] && kill -0 "$(cat "$CAFFEINATE_PID")" 2>/dev/null; then
    ok "caffeinate already running (pid $(cat "$CAFFEINATE_PID"))"
  else
    nohup caffeinate -di > /dev/null 2>&1 &
    echo $! > "$CAFFEINATE_PID"
    disown
    ok "caffeinate spawned (pid $(cat "$CAFFEINATE_PID"))"
  fi
else
  warn "caffeinate not available — skipping"
fi

# ─── 9. Open browser ──────────────────────────────────────────────────
hdr "opening browser"
if command -v open > /dev/null; then
  open "$FRONTEND_URL"
  ok "opened $FRONTEND_URL"
else
  warn "open command not found — point your browser at $FRONTEND_URL"
fi

# ─── 10. Summary ──────────────────────────────────────────────────────
echo
echo "${C_BOLD}${C_GREEN}══════════════════════════════════════════════════${C_RESET}"
echo "${C_BOLD}${C_GREEN}  VolX is live                                   ${C_RESET}"
echo "${C_BOLD}${C_GREEN}══════════════════════════════════════════════════${C_RESET}"
echo
echo "  ${C_BOLD}Frontend${C_RESET}    $FRONTEND_URL"
echo "  ${C_BOLD}BVOL chart${C_RESET}  $FRONTEND_URL/chart/bvol"
echo "  ${C_BOLD}EVOL chart${C_RESET}  $FRONTEND_URL/chart/evol"
echo "  ${C_BOLD}Methodology${C_RESET} $FRONTEND_URL/methodology"
echo
echo "  ${C_BOLD}API health${C_RESET}  ${TUNNEL_BASE}/v1/health"
echo "  ${C_BOLD}BVOL JSON${C_RESET}   ${TUNNEL_BASE}/v1/index/bvol/latest"
echo
echo "${C_DIM}  next-dev log:  $FRONTEND_LOG${C_RESET}"
echo "${C_DIM}  tear down:     scripts/demo.sh --down${C_RESET}"
echo "${C_DIM}  status check:  scripts/demo.sh --status${C_RESET}"
echo
