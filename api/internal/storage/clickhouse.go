// Package storage wraps the ClickHouse + Redis clients used by every
// handler. Kept thin: the drivers themselves are already pool-managed
// and concurrency-safe — these wrappers exist to centralise dialing,
// timeouts, and the queries the handlers share (`LastIndexTickAge`,
// hot-path lookups).
package storage

import (
	"context"
	"fmt"
	"regexp"
	"time"

	"github.com/ClickHouse/clickhouse-go/v2"
	"github.com/ClickHouse/clickhouse-go/v2/lib/driver"
)

// validDBName is the allowlist for `CLICKHOUSE_DB`. Database / table
// identifiers can't be parameter-bound in `clickhouse-go/v2`, so we
// validate them once at startup rather than interpolating untrusted
// strings into every query. Pinned to letters / digits / underscores
// — ClickHouse's actual identifier grammar is wider but no real
// schema needs anything else.
var validDBName = regexp.MustCompile(`^[A-Za-z_][A-Za-z0-9_]*$`)

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
	if !validDBName.MatchString(db) {
		return nil, fmt.Errorf("clickhouse: invalid CLICKHOUSE_DB %q (must match %s)", db, validDBName)
	}
	opts, err := clickhouse.ParseDSN(dsn)
	if err != nil {
		return nil, fmt.Errorf("clickhouse: parse dsn: %w", err)
	}
	// `CLICKHOUSE_DB` is the source of truth — always override the
	// DSN's `database` field. Without this, a DSN that carried its
	// own DB would cause a split-brain: the connection dialled
	// against the DSN's DB, but our queries (built from `c.DB`)
	// would target the env's DB instead. Match-or-error would also
	// work; overriding is simpler and matches the env-wins precedence
	// the rest of the binary uses.
	opts.Auth.Database = db
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
// the engine has run a tick). The clickhouse-go driver maps a NULL
// `max(ts)` to Unix epoch zero (1970-01-01), not Go's
// `time.Time{}`, so the empty-table guard checks `Unix() <= 0`
// rather than `IsZero()`.
func (c *ClickHouse) LastIndexTickAge(ctx context.Context) (time.Duration, error) {
	var maxTs time.Time
	q := fmt.Sprintf("SELECT max(ts) FROM %s.index_ticks", c.DB)
	if err := c.Conn.QueryRow(ctx, q).Scan(&maxTs); err != nil {
		return 0, fmt.Errorf("clickhouse: last index tick: %w", err)
	}
	if maxTs.IsZero() || maxTs.Unix() <= 0 {
		return 0, nil
	}
	return time.Since(maxTs), nil
}

// Close releases the underlying pool.
func (c *ClickHouse) Close() error { return c.Conn.Close() }
