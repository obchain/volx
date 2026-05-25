-- VolX ClickHouse schema (issue #15, PRD §5).
--
-- Auto-applied by the official ClickHouse image when this file is mounted
-- at `/docker-entrypoint-initdb.d/`. Runs once on the first cold init of
-- the data volume; re-running requires `docker compose down -v`.
--
-- Three storage layers:
--   options_ticks    raw per-leg quotes from each venue (write-heavy)
--   index_ticks      published BVOL / EVOL rows (one per 60 s slot)
--   index_1m         AggregatingMergeTree OHLC rollup of index_ticks
--                    fed by the index_1m_mv materialized view
--
-- Column types intentionally mirror `crates/shared-types/src/{tick,index}.rs`
-- so the normalizer (#16) and engine (#20) write Rust structs in directly
-- via the ClickHouse driver — no per-field translation layer.

CREATE DATABASE IF NOT EXISTS volx;

-- ---------------------------------------------------------------------------
-- options_ticks — raw market data
-- ---------------------------------------------------------------------------
--
-- ORDER BY (asset, expiry, strike, ts):
--   queries always filter by (asset, expiry) at minimum (engine strip
--   builder), often also (strike). `ts` last so per-strike scans stay
--   chronological.
--
-- PARTITION BY toYYYYMM(ts):
--   monthly partitions give a clean drop boundary for the TTL.
--
-- TTL 1 year per PRD §5:
--   raw ticks aren't needed beyond a year for backtests + audit;
--   index_ticks survives independently.
--
-- ZSTD(3) codec on the wide float columns:
--   ~15-20× compression ratio over uncompressed Float64 on the
--   measured tick stream. Level 3 is the standard speed / ratio knee.

CREATE TABLE IF NOT EXISTS volx.options_ticks
(
    venue          LowCardinality(String),                     -- deribit / okx / bybit
    asset          LowCardinality(String),                     -- btc / eth
    expiry         DateTime64(3, 'UTC'),
    strike         Float64                          CODEC(ZSTD(3)),
    kind           LowCardinality(String),                     -- call / put
    bid            Nullable(Float64)                CODEC(ZSTD(3)),
    ask            Nullable(Float64)                CODEC(ZSTD(3)),
    mid            Nullable(Float64)                CODEC(ZSTD(3)),
    iv             Nullable(Float64)                CODEC(ZSTD(3)),
    underlying     Float64                          CODEC(ZSTD(3)),
    open_interest  Float64                          CODEC(ZSTD(3)),
    volume_24h     Float64                          CODEC(ZSTD(3)),
    received_at    DateTime64(3, 'UTC'),
    -- `ts` is the canonical timestamp the engine sorts on. We default to
    -- received_at so the writer doesn't have to compute it; an explicit
    -- value can still be supplied if a venue ever publishes a separate
    -- event-time vs. ingest-time.
    ts             DateTime64(3, 'UTC')             DEFAULT received_at
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(ts)
ORDER BY (asset, expiry, strike, ts)
TTL toDate(ts) + INTERVAL 1 YEAR
SETTINGS index_granularity = 8192;

-- ---------------------------------------------------------------------------
-- index_ticks — published BVOL / EVOL rows (60 s cadence)
-- ---------------------------------------------------------------------------
--
-- No TTL: the index series is the project's permanent output. Single
-- daily-volume table (max ~1.4k rows/day at 60 s × 2 indices), so a
-- per-month partition is plenty.
--
-- `strip_hash` is FixedString(32): matches `volx_shared_types::StripHash([u8; 32])`.
-- The JSON wire format (hex64) is the API layer's concern.

CREATE TABLE IF NOT EXISTS volx.index_ticks
(
    index_id    LowCardinality(String),                       -- BVOL / EVOL
    value       Float64                             CODEC(ZSTD(3)),
    confidence  Float64                             CODEC(ZSTD(3)),
    strip_hash  FixedString(32),
    ts          DateTime64(3, 'UTC')
)
ENGINE = MergeTree
PARTITION BY toYYYYMM(ts)
ORDER BY (index_id, ts);

-- ---------------------------------------------------------------------------
-- index_1m — 1-minute OHLC rollup, fed by a materialized view trigger
-- ---------------------------------------------------------------------------
--
-- Stored as AggregateFunction states so partial rollups can be merged.
-- Frontend reads via `*Merge()` or `SELECT ... FINAL`.
--
-- Columns:
--   open  = first tick value in the minute  (argMin on ts)
--   close = last tick value in the minute   (argMax on ts)
--   high  = max(value) in the minute        (SimpleAggregateFunction)
--   low   = min(value) in the minute        (SimpleAggregateFunction)
--   count = number of ticks in the minute   (AggregateFunction)
--   avg_confidence = average confidence     (AggregateFunction)

CREATE TABLE IF NOT EXISTS volx.index_1m
(
    index_id        LowCardinality(String),
    bucket          DateTime('UTC'),
    open_state      AggregateFunction(argMin, Float64, DateTime64(3, 'UTC')),
    close_state     AggregateFunction(argMax, Float64, DateTime64(3, 'UTC')),
    high            SimpleAggregateFunction(max, Float64),
    low             SimpleAggregateFunction(min, Float64),
    count_state     AggregateFunction(count),
    avg_conf_state  AggregateFunction(avg, Float64)
)
ENGINE = AggregatingMergeTree
PARTITION BY toYYYYMM(bucket)
ORDER BY (index_id, bucket);

CREATE MATERIALIZED VIEW IF NOT EXISTS volx.index_1m_mv
TO volx.index_1m AS
SELECT
    index_id,
    toStartOfMinute(ts)              AS bucket,
    argMinState(value, ts)           AS open_state,
    argMaxState(value, ts)           AS close_state,
    max(value)                       AS high,
    min(value)                       AS low,
    countState()                     AS count_state,
    avgState(confidence)             AS avg_conf_state
FROM volx.index_ticks
GROUP BY index_id, bucket;
