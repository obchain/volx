// Package config loads runtime configuration from environment variables.
// Defaults match `docker/docker-compose.yml` + the Rust crates' env wiring
// so a single `.env` works for the entire stack.
package config

import (
	"fmt"
	"os"
	"time"
)

// Config is the API binary's resolved environment view. All fields are
// set exactly once at startup; no live reload (no consumer needs it).
type Config struct {
	// BindAddr — `host:port` for the fiber listener. Localhost-only
	// default so a misconfigured firewall can't accidentally expose
	// the API LAN-wide. Override via `API_BIND`.
	BindAddr string

	// ClickHouseDSN — native protocol DSN. `clickhouse-go/v2` accepts
	// the `clickhouse://` URL form; we keep it as a single env var
	// rather than four separate ones to minimise wiring drift between
	// this binary and the Rust crates.
	ClickHouseDSN string
	ClickHouseDB  string

	// RedisURL — `redis://host:port[/db]`. Same single-var posture.
	RedisURL string

	// Version reported on `/v1/health`. Tied to the
	// METHODOLOGY_VERSION constant in `volx-shared-types`; the
	// release pipeline (#28) bumps both together.
	Version string

	// HealthMaxAgeSecs — `/v1/health` returns `status=degraded` if
	// the most recent `index_ticks` row is older than this. 90 s is
	// 1.5 × the engine's 60 s recompute cadence.
	HealthMaxAge time.Duration

	// WSMaxConnsPerIP caps per-IP active WS connections. PRD §6
	// pins the anon limit at 5; paid-tier auth (#M3) will skip
	// this limiter for authenticated keys.
	WSMaxConnsPerIP int
}

// Load reads every env var, applying defaults that match
// `.env.example`. Returns an error only if a value is present but
// unparseable.
func Load() (*Config, error) {
	c := &Config{
		BindAddr:        env("API_BIND", "127.0.0.1:8080"),
		ClickHouseDSN:   env("CLICKHOUSE_DSN", "clickhouse://default@127.0.0.1:9000?dial_timeout=2s"),
		ClickHouseDB:    env("CLICKHOUSE_DB", "volx"),
		RedisURL:        env("REDIS_URL", "redis://127.0.0.1:6379"),
		Version:         env("VOLX_VERSION", "0.1.0"),
		HealthMaxAge:    90 * time.Second,
		WSMaxConnsPerIP: 5,
	}
	if raw := os.Getenv("WS_MAX_CONNS_PER_IP"); raw != "" {
		var n int
		if _, err := fmt.Sscanf(raw, "%d", &n); err != nil {
			return nil, fmt.Errorf("WS_MAX_CONNS_PER_IP=%q: %w", raw, err)
		}
		c.WSMaxConnsPerIP = n
	}
	if raw := os.Getenv("HEALTH_MAX_AGE_SECS"); raw != "" {
		var secs int
		if _, err := fmt.Sscanf(raw, "%d", &secs); err != nil {
			return nil, fmt.Errorf("HEALTH_MAX_AGE_SECS=%q: %w", raw, err)
		}
		c.HealthMaxAge = time.Duration(secs) * time.Second
	}
	return c, nil
}

func env(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}
