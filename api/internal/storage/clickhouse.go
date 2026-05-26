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

// OHLCRow is one bar of an aggregated index series. `Ts` is the
// bucket start (UTC). `Count` is the number of underlying 60 s
// engine ticks that landed in the bar; `AvgConfidence` is their
// average confidence value. Lightweight-charts on the frontend
// (#27) consumes `Ts / Open / High / Low / Close` directly.
type OHLCRow struct {
	Ts            time.Time `json:"ts"`
	Open          float64   `json:"open"`
	High          float64   `json:"high"`
	Low           float64   `json:"low"`
	Close         float64   `json:"close"`
	Count         uint64    `json:"count"`
	AvgConfidence float64   `json:"avg_confidence"`
}

// HistoryInterval is the validated aggregation interval. The string
// form is the wire-format value the API exposes and the SQL form is
// the `INTERVAL N MINUTE` literal we re-bucket the underlying
// `index_1m` rollup with.
type HistoryInterval struct {
	wire string
	mins int
}

// AllowedHistoryIntervals lists the buckets supported by
// `/v1/index/{id}/history`. Pinned per PRD §6 — extending requires a
// wire-format bump.
var AllowedHistoryIntervals = []HistoryInterval{
	{"1m", 1},
	{"5m", 5},
	{"1h", 60},
	{"1d", 24 * 60},
}

// ParseHistoryInterval validates the `interval` query param.
func ParseHistoryInterval(s string) (HistoryInterval, error) {
	for _, hi := range AllowedHistoryIntervals {
		if hi.wire == s {
			return hi, nil
		}
	}
	return HistoryInterval{}, fmt.Errorf("invalid interval %q (allowed: 1m, 5m, 1h, 1d)", s)
}

// IndexHistory pulls an OHLC series from the `index_1m`
// AggregatingMergeTree rollup (#15), re-bucketed to the requested
// interval. `limit` is the maximum number of bars returned, in
// chronological order (oldest first) so the chart consumer can
// `setData()` directly.
//
// `tickerID` is the ClickHouse `LowCardinality(String)` value
// (`"BVOL" / "EVOL"`). The function trusts the caller has already
// validated it against a known set (the handler does, via
// `IndexId::from_ticker`-equivalent in the handler).
//
// `index_1m` stores Aggregate* state columns; merging them happens
// inside the inner SELECT so the outer aggregation operates on
// scalar `value` doubles.
func (c *ClickHouse) IndexHistory(
	ctx context.Context,
	tickerID string,
	hi HistoryInterval,
	limit int,
) ([]OHLCRow, error) {
	if !validDBName.MatchString(c.DB) {
		// Defence-in-depth — the same guard ran at OpenClickHouse,
		// but a refactor that mutated c.DB would slip the check
		// otherwise.
		return nil, fmt.Errorf("clickhouse: invalid DB name %q", c.DB)
	}
	if limit <= 0 || limit > 10_000 {
		return nil, fmt.Errorf("clickhouse: invalid limit %d (must be 1..=10_000)", limit)
	}

	// The inner SELECT merges the AggregateFunction states into
	// per-minute scalars. The outer SELECT re-buckets those scalars
	// to the requested interval. For `1m` the outer aggregation is
	// a no-op (1-minute buckets re-grouped by 1 minute = identity).
	q := fmt.Sprintf(`
        SELECT
            toStartOfInterval(bucket, INTERVAL ? MINUTE) AS ts,
            argMin(open, bucket)                          AS open,
            max(high)                                     AS high,
            min(low)                                      AS low,
            argMax(close, bucket)                         AS close,
            sum(count_v)                                  AS cnt,
            avg(avg_conf_v)                               AS avg_conf
        FROM (
            SELECT
                bucket,
                argMinMerge(open_state)  AS open,
                argMaxMerge(close_state) AS close,
                max(high)                AS high,
                min(low)                 AS low,
                countMerge(count_state)  AS count_v,
                avgMerge(avg_conf_state) AS avg_conf_v
            FROM %s.index_1m
            WHERE index_id = ?
            GROUP BY index_id, bucket
        )
        GROUP BY ts
        ORDER BY ts DESC
        LIMIT ?
    `, c.DB)

	rows, err := c.Conn.Query(ctx, q, hi.mins, tickerID, limit)
	if err != nil {
		return nil, fmt.Errorf("clickhouse: history query: %w", err)
	}
	defer rows.Close()

	// `index_1m` query above returns rows in DESC order so a small
	// `limit` brings the newest bars. Reverse in-place at the end
	// so the wire format is oldest-first (lightweight-charts
	// requires ascending time).
	out := make([]OHLCRow, 0, limit)
	for rows.Next() {
		var r OHLCRow
		if err := rows.Scan(&r.Ts, &r.Open, &r.High, &r.Low, &r.Close, &r.Count, &r.AvgConfidence); err != nil {
			return nil, fmt.Errorf("clickhouse: history scan: %w", err)
		}
		out = append(out, r)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("clickhouse: history rows iter: %w", err)
	}
	// Reverse to ascending time.
	for i, j := 0, len(out)-1; i < j; i, j = i+1, j-1 {
		out[i], out[j] = out[j], out[i]
	}
	return out, nil
}

// Close releases the underlying pool.
func (c *ClickHouse) Close() error { return c.Conn.Close() }
