// Package handlers exposes the public REST surface.
//
// Currently:
//   - `GET /v1/health` — liveness + freshness probe (issue #22)
//
// Later (per the v1 roadmap):
//   - `GET /v1/index/{id}/latest`        — Redis hot cache  (#23)
//   - `GET /v1/index/{id}/history`       — ClickHouse range (#23)
//   - `GET /v1/options/strip?…`          — strip pull       (#23)
//   - `WS  /v1/stream`                   — pubsub fanout    (#24)
package handlers

import (
	"context"
	"time"

	"github.com/gofiber/fiber/v3"

	"github.com/obchain/volx/api/internal/storage"
)

// HealthDeps holds the wiring the handler needs. The struct shape is
// stable so a future migration to dependency injection (issue #23+)
// can plug into the same call site.
type HealthDeps struct {
	Clickhouse *storage.ClickHouse
	Redis      *storage.Redis
	Version    string
	MaxAge     time.Duration
}

// HealthResponse is the JSON body of `/v1/health`. Field order +
// names match the issue #22 spec exactly — dashboards key on them.
//
// `last_update_age_s` carries a `-1.0` sentinel when no index_ticks
// row exists yet (first boot, never-published engine). Without the
// sentinel a `degraded` status would ship alongside
// `last_update_age_s: 0.0`, which any numeric freshness alert
// (`age_s < 90` = healthy) would silently treat as the freshest
// possible reading. Consumers must check `status` first; the age
// field is only meaningful when `status ∈ {"ok", "degraded"}` *and*
// the value is non-negative.
type HealthResponse struct {
	Status           string  `json:"status"`
	LastUpdateAgeSec float64 `json:"last_update_age_s"`
	Version          string  `json:"version"`
}

// AgeSentinelNoTicks marks "no row in index_ticks yet" so a freshness
// alert keying on `last_update_age_s < N` does not flag the
// never-booted-engine case as healthy.
const AgeSentinelNoTicks = -1.0

// Health wires `GET /v1/health`.
//
// Behaviour:
//   - Both backends reachable + the most recent `index_ticks` row is
//     within `MaxAge` (default 90 s) → `status = "ok"`, HTTP 200.
//   - Backends reachable but the row is older than `MaxAge` (engine
//     is alive but not publishing) → `status = "degraded"`, HTTP 200.
//     The 200 is intentional: an upstream load balancer that strips
//     pods on non-2xx would amplify a freshness blip into a real
//     outage. The body distinguishes the case.
//   - Either backend unreachable → `status = "down"`, HTTP 503.
//
// The 1 s context timeout matches the Redis client's read timeout so
// a slow backend can't wedge the readiness probe past the orchestrator's
// 2 s default.
func Health(d HealthDeps) fiber.Handler {
	return func(c fiber.Ctx) error {
		ctx, cancel := context.WithTimeout(c.Context(), 1*time.Second)
		defer cancel()

		if err := d.Redis.Ping(ctx); err != nil {
			return c.Status(fiber.StatusServiceUnavailable).JSON(HealthResponse{
				Status:           "down",
				LastUpdateAgeSec: 0,
				Version:          d.Version,
			})
		}

		age, err := d.Clickhouse.LastIndexTickAge(ctx)
		if err != nil {
			return c.Status(fiber.StatusServiceUnavailable).JSON(HealthResponse{
				Status:           "down",
				LastUpdateAgeSec: 0,
				Version:          d.Version,
			})
		}

		status := "ok"
		ageField := age.Seconds()
		// `age == 0` happens on first boot before the engine has run
		// a single tick — report "degraded" rather than "ok" so an
		// operator notices an engine that never connected. Use the
		// `AgeSentinelNoTicks` value rather than `0.0` so the JSON
		// is not internally inconsistent (status=degraded but
		// age=0.0 — fresh as can be).
		switch {
		case age == 0:
			status = "degraded"
			ageField = AgeSentinelNoTicks
		case age > d.MaxAge:
			status = "degraded"
		}

		return c.JSON(HealthResponse{
			Status:           status,
			LastUpdateAgeSec: ageField,
			Version:          d.Version,
		})
	}
}
