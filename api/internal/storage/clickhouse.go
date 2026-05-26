// Package storage wraps the ClickHouse + Redis clients used by every
// handler. Kept thin: the drivers themselves are already pool-managed
// and concurrency-safe — these wrappers exist to centralise dialing,
// timeouts, and the queries the handlers share (`LastIndexTickAge`,
// hot-path lookups).
package storage

import (
	"context"
	"fmt"
	"time"

	"github.com/ClickHouse/clickhouse-go/v2"
	"github.com/ClickHouse/clickhouse-go/v2/lib/driver"
)

// ClickHouse is the API's read-only handle to the engine's published
// rows. Writes never originate here — the engine binary is the only
// writer to `volx.index_ticks`.
type ClickHouse struct {
	Conn driver.Conn
	DB   string
}

// OpenClickHouse dials the server and verifies connectivity with a
// `Ping`. Returns the error if either step fails so the API refuses
// to start against a broken backend (matches the Rust crates'
// fail-fast posture from #16 / #20).
func OpenClickHouse(ctx context.Context, dsn, db string) (*ClickHouse, error) {
	opts, err := clickhouse.ParseDSN(dsn)
	if err != nil {
		return nil, fmt.Errorf("clickhouse: parse dsn: %w", err)
	}
	// Force the database in case the DSN omits it; the binary always
	// operates against one logical DB.
	if opts.Auth.Database == "" {
		opts.Auth.Database = db
	}
	// Read-only connection pool sizing. The API's hot path is one
	// query per request; a 10-conn pool absorbs bursts without
	// hammering the server.
	opts.MaxOpenConns = 10
	opts.MaxIdleConns = 5
	opts.ConnMaxLifetime = 30 * time.Minute
	opts.DialTimeout = 2 * time.Second

	conn, err := clickhouse.Open(opts)
	if err != nil {
		return nil, fmt.Errorf("clickhouse: open: %w", err)
	}
	pingCtx, cancel := context.WithTimeout(ctx, 2*time.Second)
	defer cancel()
	if err := conn.Ping(pingCtx); err != nil {
		_ = conn.Close()
		return nil, fmt.Errorf("clickhouse: ping: %w", err)
	}
	return &ClickHouse{Conn: conn, DB: db}, nil
}

// LastIndexTickAge returns the age of the most recent
// `index_ticks` row. The health handler uses this to decide whether
// the engine is alive — if the engine binary stops publishing the
// number grows linearly and the handler eventually flips to
// `degraded`.
//
// Returns `(0, nil)` if the table is empty (e.g., first boot before
// the engine has run a tick).
func (c *ClickHouse) LastIndexTickAge(ctx context.Context) (time.Duration, error) {
	var maxTs time.Time
	q := fmt.Sprintf("SELECT max(ts) FROM %s.index_ticks", c.DB)
	if err := c.Conn.QueryRow(ctx, q).Scan(&maxTs); err != nil {
		return 0, fmt.Errorf("clickhouse: last index tick: %w", err)
	}
	if maxTs.IsZero() {
		return 0, nil
	}
	return time.Since(maxTs), nil
}

// Close releases the underlying pool.
func (c *ClickHouse) Close() error { return c.Conn.Close() }
