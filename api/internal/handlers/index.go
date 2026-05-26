package handlers

import (
	"context"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"
	"strings"
	"time"

	"github.com/gofiber/fiber/v3"

	"github.com/obchain/volx/api/internal/storage"
)

// Engine publish cadence per `METHODOLOGY.md` §5. Used to compute the
// `next_update_eta_seconds` field on the `/latest` response — the
// frontend's countdown clock keys on it.
const enginePublishIntervalSecs = 60

// HistoryDefaultLimit is the default `limit` when the query param is
// absent. Picks 1000 to match `lightweight-charts`' typical default
// page on the frontend (#27).
const HistoryDefaultLimit = 1000

// IndexDeps holds the wiring for every `/v1/index/*` and
// `/v1/options/*` handler. Same shape rule as `HealthDeps` — see
// `health.go`.
type IndexDeps struct {
	Clickhouse *storage.ClickHouse
	Redis      *storage.Redis
}

// indexTickers is the allowlist for the `{id}` path segment. The
// `ClickHouse` `LowCardinality(String)` column matches against the
// upper-case form (`"BVOL" / "EVOL"`); the URL accepts the
// lower-case form per PRD §6 (`/v1/index/bvol/latest`).
var indexTickers = map[string]string{
	"bvol": "BVOL",
	"evol": "EVOL",
}

func normalizeTicker(raw string) (string, bool) {
	t, ok := indexTickers[strings.ToLower(raw)]
	return t, ok
}

// --- /v1/index/{id}/latest ----------------------------------------

// latestResponse is the PRD §6 envelope.
type latestResponse struct {
	Index                string  `json:"index"`
	Value                float64 `json:"value"`
	Ts                   string  `json:"ts"`
	Confidence           float64 `json:"confidence"`
	SourceStripHash      string  `json:"source_strip_hash"`
	NextUpdateEtaSeconds int     `json:"next_update_eta_seconds"`
}

// engineIndexValue mirrors the JSON written by the Rust engine's
// `IndexSinks::publish` (#20 + #23). We keep the deserialization
// shape narrow — only fields the API actually reads.
type engineIndexValue struct {
	IndexID    string  `json:"index_id"`
	Value      float64 `json:"value"`
	Confidence float64 `json:"confidence"`
	StripHash  string  `json:"strip_hash"`
	Ts         string  `json:"ts"`
}

// IndexLatest wires `GET /v1/index/{id}/latest`.
func IndexLatest(d IndexDeps) fiber.Handler {
	return func(c fiber.Ctx) error {
		ticker, ok := normalizeTicker(c.Params("id"))
		if !ok {
			return c.Status(fiber.StatusBadRequest).JSON(fiber.Map{
				"error": "unknown index id (allowed: bvol, evol)",
			})
		}
		ctx, cancel := context.WithTimeout(c.Context(), 1*time.Second)
		defer cancel()

		raw, err := d.Redis.GetLatest(ctx, ticker)
		if err != nil {
			if errors.Is(err, storage.ErrNotFound) {
				return c.Status(fiber.StatusNotFound).JSON(fiber.Map{
					"error": fmt.Sprintf("no latest value for %s (engine has not published yet)", ticker),
				})
			}
			return c.Status(fiber.StatusServiceUnavailable).JSON(fiber.Map{
				"error": "redis lookup failed",
			})
		}

		var src engineIndexValue
		if err := json.Unmarshal([]byte(raw), &src); err != nil {
			return c.Status(fiber.StatusInternalServerError).JSON(fiber.Map{
				"error": "stored value is not parseable JSON",
			})
		}

		// `next_update_eta_seconds` = how long until the engine is
		// expected to publish the next tick. Clamp at zero so a
		// stale value (engine paused) doesn't surface a negative
		// number to the consumer's countdown UI.
		eta := enginePublishIntervalSecs
		if t, err := time.Parse(time.RFC3339Nano, src.Ts); err == nil {
			elapsed := int(time.Since(t).Seconds())
			eta = enginePublishIntervalSecs - elapsed
			if eta < 0 {
				eta = 0
			}
		}

		return c.JSON(latestResponse{
			Index:                src.IndexID,
			Value:                src.Value,
			Ts:                   src.Ts,
			Confidence:           src.Confidence,
			SourceStripHash:      formatStripHash(src.StripHash),
			NextUpdateEtaSeconds: eta,
		})
	}
}

// formatStripHash converts the engine's lowercase hex strip-hash
// (64 chars, no prefix) into the PRD §6 `0x`-prefixed form. If the
// engine ever changed encoding (e.g., base64), this is the only
// place the API needs to mirror that change.
func formatStripHash(raw string) string {
	if raw == "" {
		return ""
	}
	// Validate that it's hex and 32 bytes; if so prefix with `0x`.
	// On parse failure, return as-is so the consumer sees what the
	// store actually contains rather than a silent rewrite.
	if b, err := hex.DecodeString(raw); err == nil && len(b) == 32 {
		return "0x" + raw
	}
	return raw
}

// --- /v1/index/{id}/history ---------------------------------------

// IndexHistory wires `GET /v1/index/{id}/history?interval=1m&limit=N`.
func IndexHistory(d IndexDeps) fiber.Handler {
	return func(c fiber.Ctx) error {
		ticker, ok := normalizeTicker(c.Params("id"))
		if !ok {
			return c.Status(fiber.StatusBadRequest).JSON(fiber.Map{
				"error": "unknown index id (allowed: bvol, evol)",
			})
		}

		intervalRaw := c.Query("interval", "1m")
		hi, err := storage.ParseHistoryInterval(intervalRaw)
		if err != nil {
			return c.Status(fiber.StatusBadRequest).JSON(fiber.Map{
				"error": err.Error(),
			})
		}

		limit := HistoryDefaultLimit
		if v := c.Query("limit"); v != "" {
			n, perr := strconv.Atoi(v)
			if perr != nil || n <= 0 || n > 10_000 {
				return c.Status(fiber.StatusBadRequest).JSON(fiber.Map{
					"error": "limit must be an integer in [1, 10000]",
				})
			}
			limit = n
		}

		// 2 s ClickHouse timeout — history reads scan the
		// `index_1m` rollup which is small (~1.4k rows / day × N
		// retention days). Longer than this means the table is
		// unhealthy or the limit is being abused.
		ctx, cancel := context.WithTimeout(c.Context(), 2*time.Second)
		defer cancel()

		rows, err := d.Clickhouse.IndexHistory(ctx, ticker, hi, limit)
		if err != nil {
			return c.Status(fiber.StatusServiceUnavailable).JSON(fiber.Map{
				"error": "clickhouse history query failed",
			})
		}
		return c.JSON(fiber.Map{
			"index":    ticker,
			"interval": intervalRaw,
			"bars":     rows,
		})
	}
}

// --- /v1/options/strip --------------------------------------------

// OptionsStrip wires `GET /v1/options/strip?index=bvol`. Pure
// passthrough of the engine's `index:{id}:last_strip` Redis envelope
// — the API does not synthesise this; it would require duplicating
// the strip builder, which we deliberately avoid.
func OptionsStrip(d IndexDeps) fiber.Handler {
	return func(c fiber.Ctx) error {
		ticker, ok := normalizeTicker(c.Query("index", "bvol"))
		if !ok {
			return c.Status(fiber.StatusBadRequest).JSON(fiber.Map{
				"error": "unknown index (allowed: bvol, evol)",
			})
		}
		ctx, cancel := context.WithTimeout(c.Context(), 1*time.Second)
		defer cancel()

		raw, err := d.Redis.GetLastStrip(ctx, ticker)
		if err != nil {
			if errors.Is(err, storage.ErrNotFound) {
				return c.Status(fiber.StatusNotFound).JSON(fiber.Map{
					"error": fmt.Sprintf("no strip yet for %s", ticker),
				})
			}
			return c.Status(fiber.StatusServiceUnavailable).JSON(fiber.Map{
				"error": "redis lookup failed",
			})
		}
		// `raw` is already a JSON document (engine wrote
		// serde_json::to_string). Pass-through; fiber's content-type
		// defaults to text/plain on `SendString` so set it
		// explicitly.
		c.Set(fiber.HeaderContentType, fiber.MIMEApplicationJSON)
		return c.SendString(raw)
	}
}
