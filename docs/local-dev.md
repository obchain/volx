# Local development

Bring up the storage backends + run the Rust pipeline against them. No
cloud accounts, no API keys.

## Prerequisites

- **Docker** with `docker compose` v2 (Desktop on macOS / Windows; the
  `docker.io` distro on Linux).
- **Rust 1.85+** (`rustup default stable`).
- ~2 GB free RAM and ~1 GB disk for the ClickHouse data volume.

## 1. Start the backends

```bash
cp .env.example .env             # one-time
docker compose -f docker/docker-compose.yml up -d
```

Watch the services become healthy:

```bash
docker compose -f docker/docker-compose.yml ps
# both rows should show `(healthy)` within ~15 s
```

## 2. Smoke-test the backends

```bash
# ClickHouse: HTTP probe
curl -s 'http://127.0.0.1:8123/?query=SELECT%201'
# -> 1

# ClickHouse: native client (in-container)
docker exec -it volx-clickhouse clickhouse-client --query "SELECT version()"

# Redis
docker exec -it volx-redis redis-cli ping
# -> PONG
```

If either fails, check `docker compose logs -f` for the offending service.

## 3. Run the ingestion binary

```bash
cargo run --release -p volx-ingestion
```

This **does not yet write to ClickHouse or Redis** — that wiring lands
in issue #16. For now the binary streams Deribit ticks to a flume
channel and logs throughput every 5 s.

## 4. Wipe state

```bash
docker compose -f docker/docker-compose.yml down            # keep volumes
docker compose -f docker/docker-compose.yml down -v         # nuke volumes
```

## Notes on the compose file

- **Ports bound to `127.0.0.1` only.** A misconfigured macOS firewall
  cannot accidentally expose ClickHouse + Redis to the LAN. Production
  (M3) uses a separate compose with Caddy + Cloudflare in front.
- **No passwords in local dev.** ClickHouse runs as `default` with no
  password; Redis has no AUTH. Production reads credentials from the
  macOS Keychain at deploy time.
- **Named volumes** so a `docker compose down` (without `-v`) preserves
  data across restarts. Use `-v` to wipe.
- **Redis is capped at 256 MB** with `allkeys-lru` eviction; the
  latest-value cache + pubsub topics are bounded well below that.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| ClickHouse exits with `Too many open files` on first start | Docker Desktop file-handle limit. Restart Docker; the compose file sets `nofile=262144`. |
| Health check stuck on `(starting)` for > 1 min | Check `docker compose logs clickhouse`. First-boot schema init can be slow on cold cache. |
| `cargo run -p volx-ingestion` errors immediately | Network reach to Deribit (`api.deribit.com`). Try `curl https://www.deribit.com/api/v2/public/get_time`. |
