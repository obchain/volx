# Deployment

How VolX runs in production: the whole backend lives as a Docker Compose
stack on a single always-on server, only the API is exposed to the public
(through a Cloudflare Tunnel), the frontend is a static Next.js build on
Netlify, and images are built and shipped automatically by CI.

> Testnet demo deployment. Not audited, demo liquidity — see
> [`onchain-demo.md`](./onchain-demo.md).

---

## Topology

```
                          ┌──────────────────────────────┐
   browser ── HTTPS ────► │  Netlify (static Next.js)    │   /trade /pool /dashboard
                          └──────────────┬───────────────┘
                                         │  API_PROXY_TARGET / NEXT_PUBLIC_API_BASE
                                         ▼
   browser ── HTTPS ──► volx-api.ancilar.com  (Cloudflare Tunnel)
                                         │
                                         ▼
   ┌─────────────────────── always-on server (Docker Compose: volx-prod) ──────────────────────┐
   │                                                                                            │
   │   cloudflared ──► api:8080 ──► clickhouse:8123/9000   (private compose network only)       │
   │                      ▲              ▲                                                       │
   │                      │              │                                                       │
   │   ingestion ─────────┼──────────────┘   engine ──► redis:6379 ◄── api                      │
   │       │ ticks         │  index_ticks         │                                              │
   │       ▼               ▼                       ▼                                             │
   │   clickhouse ◄──── normalizer (in ingestion) keeper ── signs tx ──► Sepolia (VolXOracle)   │
   │                                                                                            │
   └────────────────────────────────────────────────────────────────────────────────────────┘
                                         │
                              keeper pushes BVOL/EVOL on-chain
                                         ▼
                       VolXOracle  ◄── reads ──  VolXPerpV2   (Ethereum Sepolia)
```

Only **one** port is published on the host (`127.0.0.1:8090` → `api:8080`),
and only the Cloudflare Tunnel reaches it from the internet. The databases
and the Rust metrics ports are never exposed on the server's public
interface — everything else talks over the private compose network.

---

## Containers

Compose project name: **`volx-prod`** (so everything groups under one name).
Six services, all `restart: unless-stopped`:

| Container | Image | Host port | Role |
| --- | --- | --- | --- |
| `volx-clickhouse` | `clickhouse/clickhouse-server:24.8-alpine` | none (internal) | tick + index storage, OHLC rollups |
| `volx-redis` | `redis:7-alpine` | none (internal) | hot latest-value cache + pub/sub fan-out |
| `volx-ingestion` | `…/volx-ingestion` | none | multi-venue WS connectors + normalizer → ClickHouse |
| `volx-engine` | `…/volx-engine` | none | 60 s snapshot → variance integral → publish index |
| `volx-api` | `…/volx-api` | `127.0.0.1:8090→8080` | REST + WS (the only published service) |
| `volx-keeper` | `…/volx-keeper` | none | pushes index on-chain + executes triggered orders |
| `volx-cloudflared` | `cloudflare/cloudflared:latest` | none | optional (`--profile tunnel`) containerized tunnel |

The data services have **no host ports** — reachable only as
`clickhouse:8123/9000` and `redis:6379` on the compose network. The API is
published on **loopback only** (`127.0.0.1`), so a host-installed
`cloudflared` or a local `curl` works while the API stays off the public IP.

---

## Public exposure (Cloudflare Tunnel)

The API is reached from the internet as **`https://volx-api.ancilar.com`**
via a Cloudflare named tunnel. There are two equivalent ways to run the
connector:

- **Host-installed `cloudflared`** mapping the public hostname to
  `http://localhost:8090`, or
- **Containerized** `cloudflared` service (`--profile tunnel`) mapping the
  hostname to `http://api:8080` over the compose network, with a
  `TUNNEL_TOKEN` in the `.env`.

Either way, **no inbound ports are opened** on the server firewall — the
tunnel makes an outbound connection to Cloudflare, which terminates TLS and
forwards requests in.

The frontend (Netlify) is built with `API_PROXY_TARGET` and
`NEXT_PUBLIC_API_BASE` pointed at the public API URL.

---

## Images & registry

The four app images are built and pushed to **Docker Hub**:

```
<registry>/volx-api
<registry>/volx-engine
<registry>/volx-ingestion
<registry>/volx-keeper
```

Each is tagged twice on every build: **`latest`** and the **git SHA**
(`:<sha>`) for traceability / rollback. The registry namespace is
configurable via `VOLX_REGISTRY` (compose default), and the deployed tag via
`VOLX_TAG` (default `latest`).

The server holds **no source checkout** — only `docker-compose.prod.yml`,
`clickhouse-init.sql`, `deploy.sh`, and a `.env`. It pulls pre-built images;
it never builds. (The compose file keeps a `build:` stanza too, so a local
`up -d --build` still rebuilds from source for development.)

---

## CI/CD pipeline

Two GitHub Actions workflows: **`ci.yml`** (gate) and **`deploy.yml`**
(ship).

```
push to main
   │
   ▼
ci.yml ─ lint + test (Rust workspace, Go API, keeper) + e2e
   │  on success (workflow_run)
   ▼
deploy.yml
   ├─ build job (matrix: api · engine · ingestion · keeper)
   │     buildx → push <registry>/volx-<svc>:latest and :<sha>  (gha cache)
   │
   └─ deploy job
         ├─ install cloudflared, configure SSH over Cloudflare Access
         ├─ scp docker-compose.prod.yml + clickhouse-init.sql + deploy.sh
         ├─ write .env on the server from GitHub secrets (umask 077 → 0600)
         └─ run deploy.sh  (pull + up -d, recreate only changed services)
```

Key properties:

- **Gated** — `deploy` only runs after a green `ci` on `main` (or a manual
  `workflow_dispatch`). A red build never deploys.
- **Serialized** — `concurrency: deploy-prod, cancel-in-progress: false`, so
  deploys queue and never half-apply by racing each other.
- **Pull-only on the server** — the deploy job ships the compose + scripts
  and triggers a pull; the server never checks out source or builds.
- **SSH over Cloudflare Access** — the deploy job reaches the server through
  the same Cloudflare tunnel (no public SSH port); the key is wiped from the
  runner afterwards.

### Manual deploy

- **From CI:** trigger the `deploy` workflow via `workflow_dispatch`.
- **On the server:** `cd` to the deploy dir and run `./deploy.sh`
  (`./deploy.sh --tunnel` to also (re)start the containerized tunnel).

---

## `deploy.sh` (server-side)

Idempotent pull-and-apply. In order, it:

1. Sources the local `.env` (so `DOCKER_USERNAME/PASSWORD` are available for
   an optional `docker login` — only needed if the Docker Hub repos are
   private; public repos pull anonymously).
2. `docker compose pull` — fetches the images for the configured
   `VOLX_REGISTRY` / `VOLX_TAG`.
3. `docker compose up -d --remove-orphans` — diffs desired vs running state
   and **recreates only the services whose image (or config) changed**;
   unchanged services keep running untouched.
4. `docker image prune -f` — drops dangling old layers.
5. `docker compose ps` — prints the resulting state.

---

## Configuration & secrets

Runtime config lives in a **`.env` beside the compose file** on the server
(gitignored; written by CI from GitHub secrets, mode `0600`). Compose
auto-loads it.

| Variable | Used by | Notes |
| --- | --- | --- |
| `SEPOLIA_RPC_URL` | keeper | Sepolia JSON-RPC endpoint (required) |
| `PRIVATE_KEY` | keeper | oracle/order signer key — **testnet only** (required) |
| `VOLX_REGISTRY` | compose | Docker Hub namespace (default in compose) |
| `VOLX_TAG` | compose | deployed image tag (default `latest`) |
| `DOCKER_USERNAME` / `DOCKER_PASSWORD` | deploy.sh | only if registry repos are private |
| `TUNNEL_TOKEN` | cloudflared | only with `--profile tunnel` |

The keeper's contract addresses (`ORACLE_ADDRESS`, `PERP_ADDRESS`) are baked
into the compose file so the image needs no deployment artifact; override
them there on a redeploy.

**GitHub Actions secrets** mirror what the server needs, plus the SSH path:
`DOCKER_USERNAME`, `DOCKER_PASSWORD`, `SERVER_SSH_KEY` (+ optional
`SERVER_KNOWN_HOSTS`), `SEPOLIA_RPC_URL`, `PRIVATE_KEY`.

> Rotating the RPC or signer key means updating **both** the GitHub secret
> (so the next deploy doesn't overwrite it) **and** the server `.env`, then
> restarting the keeper. Changing only one will drift.

---

## On-chain keeper

The keeper is the bridge between the off-chain index and the on-chain perp:

- Polls the VolX API and **pushes BVOL/EVOL to `VolXOracle`** on Sepolia on
  a price-deviation trigger (`DEVIATION_BPS=50`, i.e. 0.5 %) or a heartbeat
  (`HEARTBEAT_MS=1800000`, 30 min), whichever comes first.
- Sweeps open conditional orders and **executes** limit/TP/SL when the
  oracle price crosses the trigger.
- Talks to the API over the private compose network (`http://api:8080`), and
  to Sepolia via `SEPOLIA_RPC_URL`, signing with `PRIVATE_KEY`.

`VolXPerpV2` reads the oracle with a 1-hour staleness guard, so if the
keeper stalls (e.g. an exhausted RPC quota) on-chain trades are blocked
until fresh pushes resume.

---

## Local production-stack run

To bring the full prod stack up locally (builds from source instead of
pulling):

```bash
set -a; . .secrets/sepolia.env; set +a       # SEPOLIA_RPC_URL, PRIVATE_KEY
docker compose -f docker/docker-compose.prod.yml up -d --build
curl 127.0.0.1:8090/v1/index/bvol/latest
```

For the lighter dev stack (storage + services, no keeper), use
`docker/docker-compose.yml` instead — see the README Quickstart.
