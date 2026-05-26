// volx-api — public REST + WebSocket surface for the VolX index.
//
// Roadmap (M1):
//   - #22 fiber skeleton + `/v1/health`                  ← this PR
//   - #23 REST endpoints (`/v1/index/...`, `/v1/options/...`)
//   - #24 `WS /v1/stream` live broadcast
//
// The binary reads from the same `volx.options_ticks` / `index_ticks`
// tables in ClickHouse and the same `index:{id}:latest` keys in Redis
// that the engine (#20) writes. No writes originate here.
//
// Environment variables:
//
//	API_BIND              host:port for the fiber listener (default 127.0.0.1:8080)
//	CLICKHOUSE_DSN        clickhouse://user@host:9000/?…   (default localhost)
//	CLICKHOUSE_DB         logical database                (default volx)
//	REDIS_URL             redis://host:6379                (default localhost)
//	VOLX_VERSION          reported on /v1/health           (default 0.1.0)
//	HEALTH_MAX_AGE_SECS   degraded threshold               (default 90)
package main

import (
	"context"
	"errors"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/gofiber/fiber/v3"

	"github.com/obchain/volx/api/internal/config"
	"github.com/obchain/volx/api/internal/handlers"
	"github.com/obchain/volx/api/internal/storage"
)

func main() {
	// `slog` JSON to stdout.
	//
	// The Rust crates emit JSON via `tracing_subscriber::fmt().json()`
	// with keys `timestamp / level / fields.message / target`. Go's
	// `slog` JSON handler emits `time / level / msg`. The two shapes
	// are intentionally *not* identical — the unified Vector / Loki
	// parser pipeline (#28) has separate field-renaming rules per
	// source. Both encodings are still one-event-per-line JSON, which
	// is what matters for ingestion at all.
	logger := slog.New(slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{}))
	slog.SetDefault(logger)

	cfg, err := config.Load()
	if err != nil {
		slog.Error("config load failed", "error", err)
		os.Exit(1)
	}
	slog.Info("volx-api starting",
		"version", cfg.Version,
		"bind", cfg.BindAddr,
		"clickhouse_db", cfg.ClickHouseDB,
	)

	bootCtx, bootCancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer bootCancel()

	ch, err := storage.OpenClickHouse(bootCtx, cfg.ClickHouseDSN, cfg.ClickHouseDB)
	if err != nil {
		slog.Error("clickhouse connect failed", "error", err)
		os.Exit(1)
	}
	defer func() {
		if err := ch.Close(); err != nil {
			slog.Warn("clickhouse close error", "error", err)
		}
	}()

	rds, err := storage.OpenRedis(bootCtx, cfg.RedisURL)
	if err != nil {
		slog.Error("redis connect failed", "error", err)
		os.Exit(1)
	}
	defer func() {
		if err := rds.Close(); err != nil {
			slog.Warn("redis close error", "error", err)
		}
	}()

	app := fiber.New(fiber.Config{
		AppName: "volx-api",
		// 5 s read / write so a slow client can't wedge a handler;
		// the engine publishes every 60 s so anything longer is a
		// stuck connection by definition.
		ReadTimeout:  5 * time.Second,
		WriteTimeout: 5 * time.Second,
	})

	// Versioned routes; the `v1` prefix matches PRD §6 and the
	// frontend's expectations (#25).
	v1 := app.Group("/v1")
	v1.Get("/health", handlers.Health(handlers.HealthDeps{
		Clickhouse: ch,
		Redis:      rds,
		Version:    cfg.Version,
		MaxAge:     cfg.HealthMaxAge,
	}))

	// Run the listener in its own goroutine so `main` can wait on
	// signal + shutdown sequentially.
	errCh := make(chan error, 1)
	go func() {
		slog.Info("fiber listener up", "bind", cfg.BindAddr)
		errCh <- app.Listen(cfg.BindAddr)
	}()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, os.Interrupt, syscall.SIGTERM)

	select {
	case sig := <-sigCh:
		slog.Info("signal received, shutting down", "signal", sig.String())
	case err := <-errCh:
		// `Listen` returns `http.ErrServerClosed` on graceful exit
		// which is not an actual error — flag only the real ones.
		if err != nil && !errors.Is(err, http.ErrServerClosed) {
			slog.Error("listener exited with error", "error", err)
			os.Exit(1)
		}
		return
	}

	shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := app.ShutdownWithContext(shutdownCtx); err != nil {
		slog.Error("shutdown error", "error", err)
		os.Exit(1)
	}
}
