<h1 align="center">VolX</h1>

<p align="center">
  <b>Open, exchange-neutral crypto volatility index.</b><br>
  The CBOE VIX for Bitcoin and Ethereum — reproducible, auditable, deterministic.
</p>

<p align="center">
  <a href="./METHODOLOGY.md">Methodology</a> ·
  <a href="#quickstart">Quickstart</a> ·
  <a href="#api-planned">API</a> ·
  <a href="#roadmap">Roadmap</a>
</p>

---

## Contents

- [What is VolX](#what-is-volx)
- [Why it exists](#why-it-exists)
- [Indices published](#indices-published)
- [How it works](#how-it-works)
- [Methodology in one screen](#methodology-in-one-screen)
- [Multi-venue robustness](#multi-venue-robustness)
- [Confidence score](#confidence-score)
- [Architecture](#architecture)
- [Tech stack](#tech-stack)
- [Quickstart](#quickstart)
- [Repo layout](#repo-layout)
- [API (planned)](#api-planned)
- [Data sources](#data-sources)
- [Performance targets](#performance-targets)
- [Project status](#project-status)
- [Roadmap](#roadmap)
- [Comparison to alternatives](#comparison-to-alternatives)
- [Reference implementations](#reference-implementations)
- [Security](#security)

---

## What is VolX

VolX is a **30-day constant-maturity implied-volatility index** for crypto
assets, computed from live option chains and published every 60 seconds. It
applies the **CBOE variance-swap replication methodology** (the standard
behind the VIX) to Bitcoin and Ethereum options, with adaptations specific
to crypto's listed strike grid and quoting conventions.

A single number per asset answers the same question the VIX answers for
equities: **how much volatility does the option market expect over the next
30 days?**

VolX is:

- **Methodology-locked.** The spec is the source of truth
  ([`METHODOLOGY.md`](./METHODOLOGY.md)). Any change that affects a
  published value bumps `METHODOLOGY_VERSION` (semver). Historical values
  are never rewritten.
- **Bit-for-bit reproducible.** Same inputs → same `f64` output across
  engine instances. Determinism is a design constraint, not an aspiration.
- **Auditable.** Every published row carries a content-hash of the strip
  set that produced it (`strip_hash`), so any value can be reproduced from
  the raw tick archive.
- **Exchange-neutral.** Starts with Deribit (dominant BTC + ETH options
  venue); multi-venue median blending lands in M2.
- **Self-hostable.** No paid data feeds, no API keys required to run the
  ingestion locally, no proprietary math.

---

## Why it exists

Crypto options markets have existed since 2016, but the implied-volatility
landscape is fragmented:

- **Deribit's DVOL** is the closest analogue — but it is calculated on
  Deribit's inverse contract spec, published only by Deribit, and not
  reproducible from public docs alone.
- **CBOE Bitcoin Volatility Index (BVIX)** was a paid product,
  discontinued.
- **T3 / Volmex** charge for index data via institutional licenses.

There is no **open, transparent, reproducible** 30-day implied-vol index
for crypto comparable to what the VIX is for SPX. VolX fills that gap. The
math is the CBOE white paper, adapted to crypto's listed strike grid;
everything needed to verify, reproduce, or fork the index ships in this
repo.

---

## Indices published

| Symbol | Underlying | Source venue (v1) | Status |
| --- | --- | --- | --- |
| **`BVOL`** | Bitcoin (BTC) | Deribit | Live in M1 |
| **`EVOL`** | Ethereum (ETH) | Deribit | Live in M1 |
| `SVOL` | Solana (SOL) | TBD | Stretch — depends on venue listing depth |

Each index is published as `100 · √σ²_30d` — annualised expected volatility
in percentage points (e.g. `BVOL = 65.42` means 65.42 % annualised expected
vol over the next 30 days).

---

## How it works

Every 60 seconds the engine takes a synchronised snapshot of every BTC +
ETH option, then:

1. **Selects two expiries** that bracket the 30-day target — `Near` (the
   largest listed expiry within `[7, 30)` days) and `Next` (smallest > 30
   days). If no pair satisfies both, the snapshot publishes `null` rather
   than carrying forward a stale value.
2. **Solves for the forward** via put-call parity at the strike where call
   and put mids are closest.
3. **Fits the IV surface** as a natural cubic spline in log-moneyness
   `x = ln(K / F)` — VolX's specific adaptation to the coarse crypto strike
   grid. The fitted curve is sampled on an 801-point dense grid between
   `K_min` and `K_max`, then clamped to `[1e-4, 5.0]`.
4. **Replicates the 30-day variance swap** via the Carr-Madan integral
   `(2 e^{rT} / T) · ∫ Q(K) / K² dK − (F / K₀ − 1)² / T`, evaluated by
   trapezoidal quadrature on the dense grid.
5. **Interpolates** in total-variance space (not in vol, not in annualised
   variance) to the exact 30-day point.
6. **Publishes** `BVOL = 100 · √σ²_30d` to ClickHouse + Redis + the REST/WS
   API.

`r = 0` (matches DVOL convention; the 30-day discount factor is within
0.5 % of 1 in current rate environments, and a venue-time-varying source
would add operational surface without measurable accuracy gain).

---

## Methodology in one screen

> Full spec in [`METHODOLOGY.md`](./METHODOLOGY.md). What follows is the
> onboarding summary.

| Step | Math |
| --- | --- |
| Forward `F` | `K* + e^{rT} (C(K*) − P(K*))`, `K* = argmin_K |C − P|` |
| `K₀` | Largest dense-grid strike at or below `F` |
| OTM kernel | `Q(K) = P(K)` for `K < F`, `C(K)` for `K > F`, `(P + C)/2` at `K₀` |
| Per-expiry variance | `σ²_T = (2 e^{rT} / T) · trapezoid(Q(K) / K²) − (F/K₀ − 1)² / T` |
| 30-day interp (minutes) | `w₁ = (N_T₂ − N₃₀) / (N_T₂ − N_T₁)`, weighted in total variance |
| Index value | `BVOL = 100 · √σ²_30d` |

**Filters (engine-layer, per snapshot):**
- Drop strike if `mark_iv ≤ 0.001` or non-finite.
- Drop strike if either call or put leg is missing for that `(strike, expiry)`.
- Reject the expiry if fewer than 5 strikes survive.

**Filters (normalizer-layer, per tick — live mode only):**
- Drop quote if last tick > 5 s old.
- Drop quote if `ask ≤ bid`.
- Drop quote if `(ask − bid) / mid > 0.30`.
- Drop quote if mid < intrinsic value (`1e-9` tolerance).

---

## Multi-venue robustness

VolX ingests options data from **three venues** — Deribit, OKX, and Bybit
— and blends them into a single published number per asset.

### Per-venue strip + median blend

Each 60-second snapshot is run **independently per venue**: the engine
builds a complete strip and computes a venue-local σ²₃₀d for each of the
three sources. The three values are then combined by taking the **median**
(not the mean) — so a single bad venue cannot drag the published index.
For two surviving venues the median collapses to the simple mean of the
two; for one surviving venue the index passes the single value through
unchanged.

The wire-format index hash (`strip_hash`) is a content-hash over the
multi-venue strip set, separator-delimited per venue, so any historical
index value can be reproduced bit-for-bit from the raw tick archive.

### Outlier drop policy

If a venue's σ²₃₀d deviates from the cross-venue median by more than
**5 %** for **5 consecutive 60-second ticks** (5 minutes), the engine
**drops that venue** and recomputes on the remaining two until the venue's
quotes return to consensus. Constants live in `crates/engine/src/outlier.rs`:

```
DEFAULT_THRESHOLD_PCT   = 0.05        // 5%
DEFAULT_STREAK_REQUIRED = 5           // ticks
```

**Availability rollback.** If the drop would leave the active set empty
(e.g. all three venues simultaneously diverging) the policy reverts: every
venue is kept active and the lower confidence score (see below) signals
the degraded state to downstream consumers. The system never publishes a
`null` for a transient quorum collapse.

Per-venue drops and restorations are surfaced via two Prometheus metrics:
- `volx_engine_active_venues{index}` — gauge, current count.
- `volx_engine_outlier_drops_total{index, venue, action="drop"|"restore"}`
  — counter, monotonic over the engine's lifetime.

### Why three venues, not two

Volmex BVIV/EVIV blends two venues (Deribit + OKX). With only two sources
no real outlier policy is possible — drop one and you are a single-venue
index. VolX's third venue (Bybit) is **specifically what makes the
5%/5-tick drop policy viable**: when one venue diverges, the remaining
two still constitute a quorum and the median is well-defined.

---

## Confidence score

Every published index tick carries a **`confidence ∈ [0.0, 1.0]`** value
computed from three multiplied factors:

```
confidence = venue_factor × freshness_factor × strike_factor
```

| Factor | Definition |
| --- | --- |
| `venue_factor` | `venues_live / venues_expected` (e.g. 2/3 = 0.667 if one venue is dropped) |
| `freshness_factor` | `max(0, 1 − max_quote_age / FRESH_BUDGET_S)`, `FRESH_BUDGET_S = 60` |
| `strike_factor` | `min(1, strip_strikes / METHODOLOGY_MIN_STRIKES)`, `METHODOLOGY_MIN_STRIKES = 8` |

A perfect snapshot (3/3 venues, all quotes fresh, ≥ 8 strikes) yields
`confidence = 1.0`. Any degradation in venue coverage, quote freshness, or
strike depth pulls the score below 1.0 — proportionally and
multiplicatively, so two simultaneous degradations compound rather than
mask each other.

The score is published alongside every index tick (`index_ticks.confidence`
column + REST `/v1/index/{symbol}/latest` payload + WebSocket frames) so
downstream consumers can filter, gate, or weight by it. Implementation
lives in `crates/engine/src/confidence.rs`.

---

## Architecture

```
                ┌──────────────────────────────────────────────────────┐
                │                    venue layer                       │
   Deribit WS ──┤   per-venue tokio task                               │
   OKX WS    ──┤   • reconnect + exponential backoff (1→2→4→8→16s)    │
   Bybit WS  ──┤   • per-venue isolation (panic in one ≠ others die)  │
                └──────────────────────┬───────────────────────────────┘
                                       │ OptionTick stream (flume MPSC)
                                       ▼
                ┌──────────────────────────────────────────────────────┐
                │                  normalizer                          │
                │   • staleness / spread / intrinsic / zero-bid drops  │
                │   • dedupe on (venue, instrument, ts)                │
                │   • persist to ClickHouse + Redis pubsub fanout      │
                └──────────────────────┬───────────────────────────────┘
                                       │ options_ticks
                                       ▼
                ┌──────────────────────────────────────────────────────┐
                │                   engine (60s cron)                  │
                │   ① snapshot all venues                              │
                │   ② per-venue strip (forward via parity, K₀, OTM Q)  │
                │   ③ fitted-IV spline + dense-grid resample           │
                │   ④ Carr-Madan variance integral, per venue          │
                │   ⑤ 30-day total-variance interpolation, per venue   │
                │   ⑥ median blend across venues                       │
                │   ⑦ outlier drop (5% · 5-tick streak · rollback)     │
                │   ⑧ confidence = venue × freshness × strikes         │
                │   ⑨ publish IndexValue { value, confidence, hash }   │
                └──────────────────────┬───────────────────────────────┘
                                       │ index_ticks
                                       ▼
                ┌──────────────────────────────────────────────────────┐
                │                   Go API (Fiber)                     │
                │   • REST: /v1/{latest,history,options/strip}         │
                │   • WS:   /v1/stream                                 │
                │   • Prometheus exposition + auth-keyed rate limit    │
                └──────────────────────┬───────────────────────────────┘
                                       ▼
                         ┌─────────────────────────┐
                         │  Next.js 15 frontend    │
                         │  lightweight-charts UI  │
                         │  methodology page       │
                         └─────────────────────────┘
```

Per-bar latency budget (M1): **< 1 s wall-clock** from snapshot start to
publish — well under the 60 s cadence, even at M2 multi-venue load.

---

## Tech stack

| Layer | Tech | Why |
| --- | --- | --- |
| Ingestion + engine + normalizer | **Rust 1.89** (edition 2024, resolver 3) | `f64` determinism, zero-allocation tick paths, no GC pauses inside the 60 s loop |
| Async runtime | `tokio` + `tokio-tungstenite` (rustls) | mature WS stack; rustls avoids OpenSSL on Oracle Cloud |
| In-process channel | `flume` | bounded MPSC, no `std::sync::mpsc` allocation overhead |
| Storage (ticks + index) | **ClickHouse 24.x** | 15-20× compression on option ticks; sub-second range scans |
| Cache + pubsub | **Redis 7** | hot latest-value reads + WS fanout |
| API | **Go 1.25** + Fiber v3 + gorilla/websocket + prometheus/client_golang | fast HTTP, mature WS, low ops surface |
| Frontend | **Next.js 15** (app router) + React 19 + Tailwind 4 + `lightweight-charts` | static-friendly, deterministic chart rendering |
| Research | Python 3.14 + numpy + pandas + scipy + matplotlib + jupyter | the methodology was validated here before any Rust was written |
| Lint policy | `unsafe_code = forbid`, clippy pedantic, `cargo fmt --check` | financial code; zero tolerance for memory bugs |
| Deploy (M3) | Oracle Cloud Always Free + Cloudflare + Vercel Hobby + GHCR | $0/mo recurring; ~$1/yr domain |

Determinism, free-tier compatibility, and operational simplicity are the
three non-negotiables.

---

## Quickstart

### Full pipeline in Docker (recommended)

Brings up storage (ClickHouse + Redis) **and** every service (ingestion,
engine, API) in one command. No `cargo` / `go` toolchain required on the
host:

```bash
git clone https://github.com/obchain/volx.git
cd volx
docker compose -f docker/docker-compose.yml up --build
```

First build is ~5–10 min (cargo-chef warming the dep cache); subsequent
builds skip the warm cache and finish in seconds. After ~120 s:

- `curl 127.0.0.1:8090/v1/index/bvol/latest` returns a populated payload.
- `curl localhost:9100/metrics` and `localhost:9101/metrics` expose the
  ingestion + engine Prometheus surfaces.

Stop with `Ctrl-C`. `docker compose down` keeps the ClickHouse + Redis
data volumes; add `-v` to wipe them.

### Hot-reload dev (Rust on host)

Requires **Rust 1.89+** (edition 2024, resolver 3). No API keys, no
database — the ingestion binary runs straight from a fresh clone:

```bash
git clone https://github.com/obchain/volx.git
cd volx

# Live Deribit tick stream (≈500-1000 ticks/s sustained)
cargo run --release -p volx-ingestion
```

Sample output (06:40 UTC, off-peak):

```
INFO volx-ingestion starting version="0.1.0"
INFO fetched instruments asset=Btc count=930
INFO fetched instruments asset=Eth count=746
INFO connecting to Deribit WS total=1676
INFO subscriptions sent batches=17
INFO throughput total=2924 window_rate_per_s="575.3" window_count=2924 ...
INFO throughput total=4599 window_rate_per_s="331.6" window_count=1675 ...
INFO throughput total=7735 window_rate_per_s="621.1" window_count=3136 ...
```

`Ctrl-C` exits. Uses the public `.100ms` ticker channel (no auth required;
`.raw` is gated behind an API key and is deferred to a later milestone).

### Tests + lint

```bash
cargo test   --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt    --check
```

Acceptance bar for engine correctness (M1):
**Rust engine must match the fitted-IV Python reference within `1e-6` in
`σ²_30d`** — roughly one part per million in published BVOL. Enforced by
`cargo test -p volx-engine` against snapshot fixtures.

### End-to-end smoke

`scripts/e2e-smoke.sh` boots every M1 service in dependency order, waits
two engine snapshots, and asserts that fresh data has reached the API
surface. The M1 close gate (issue #66).

```bash
./scripts/e2e-smoke.sh
```

Requirements on `PATH`: `docker`, `cargo`, `go`, `curl`, `python3` with
the `websockets` package (`pip install websockets`).

Asserts, in order:

1. `options_ticks` has ≥ 1 fresh row in the last 1 minute (ingestion +
   normalizer reaching ClickHouse).
2. `index_ticks` has ≥ 1 fresh row in the last 2 minutes (engine writing).
3. `GET /v1/index/bvol/latest` returns 200 with `value > 0` and
   `age < 150 s` (two engine cycles + slack).
4. `GET /v1/index/bvol/history?interval=5m&limit=12` returns 200 with
   `bars ≥ 1`.
5. `ws://…/v1/stream` delivers at least one `type:tick` frame for both
   `bvol` and `evol` inside a 75 s window (via
   `scripts/e2e-ws-client.py`).

Exits 0 on success with a stage-timing summary; non-zero with the name
of the failed assertion. Compose teardown runs in the exit trap so the
script is idempotent across repeated runs.

---

## Repo layout

```
volx/
├── Cargo.toml                       # Rust workspace, edition 2024, resolver 3
├── METHODOLOGY.md                   # canonical math spec — single source of truth
├── README.md
│
├── crates/                          # Rust workspace
│   ├── shared-types/                # OptionTick, Strip, IndexValue, enums — pure data
│   ├── ingestion/                   # per-venue WS connectors
│   │   └── src/venues/{deribit,okx,bybit}.rs
│   ├── normalizer/                  # tick filters + persistence (M1)
│   └── engine/                      # strip builder, variance integral, scheduler (M1)
│       └── src/{strip,variance,interpolate}.rs
│
├── api/                             # Go (Fiber) — M1
│   ├── go.mod
│   ├── cmd/api/main.go
│   ├── internal/handlers/
│   ├── internal/storage/{clickhouse,redis}.go
│   └── internal/stream/ws.go
│
├── frontend/                        # Next.js 15 — M1
│   ├── package.json
│   ├── app/page.tsx                 # landing
│   ├── app/chart/[index]/page.tsx
│   ├── app/methodology/page.tsx
│   └── app/api/page.tsx
│
├── research/                        # Python notebooks — M0 (validated math)
│   ├── vix-spx-replication.ipynb
│   ├── bvol-backtest.ipynb
│   └── bvol-dvol-gap-diagnostics.ipynb
│
├── docker/
│   ├── docker-compose.yml           # local dev: ClickHouse + Redis
│   └── docker-compose.prod.yml      # Oracle Cloud (M3)
│
├── deploy/                          # M3
│   ├── oracle-cloud-init.sh
│   └── grafana-dashboards/
│
└── docs/
    ├── vix-notes.md                 # CBOE white paper notes (math reference)
    ├── runbook.md
    └── incident-response.md
```

---

## API (planned)

The Go API is M1 scope. Endpoints below are the planned public shape.

### REST

```
GET  /v1/health                       liveness probe
GET  /v1/index/{symbol}/latest        current value + confidence + ts
GET  /v1/index/{symbol}/history       ?from=ISO&to=ISO&interval=1m
GET  /v1/options/strip                ?venue=deribit&asset=BTC&expiry=2026-06-30
```

Example:

```bash
curl https://api.volx.dev/v1/index/BVOL/latest
```

```json
{
  "index_id":    "BVOL",
  "value":       65.42,
  "confidence":  0.97,
  "strip_hash":  "9c7a…",
  "ts":          "2026-05-25T12:00:00Z"
}
```

### WebSocket

```
wss://api.volx.dev/v1/stream
```

Subscribe to one or more indices; receive `IndexValue` rows pushed on
every 60-second publish.

```json
> {"action":"subscribe","channels":["BVOL","EVOL"]}
< {"type":"index","data":{"index_id":"BVOL","value":65.42,"ts":"..."}}
< {"type":"index","data":{"index_id":"EVOL","value":71.10,"ts":"..."}}
```

Free tier: 60 req/min REST, 1 concurrent WS. M2 introduces auth-keyed
higher-tier limits.

---

## Data sources

| Need | Source | Cost | Frequency |
| --- | --- | --- | --- |
| BTC + ETH options (live) | Deribit WS `ticker.{instrument}.100ms` | $0 | sub-second |
| BTC + ETH options (live, future) | OKX + Bybit WS | $0 | sub-second |
| Underlying spot / index | Deribit `deribit_price_index.{btc,eth}_usd` | $0 | tick |
| BTC + ETH options (historical, M0 backtest) | Tardis.dev first-of-month CSV (free tier) | $0 | day snapshots |
| Deribit DVOL benchmark | Deribit `/public/get_volatility_index_data` | $0 | hourly |
| Published VIX (CBOE benchmark) | CBOE daily close CSV | $0 | daily |
| Risk-free rate `r` | constant 0 (matches DVOL; see METHODOLOGY §4.4) | n/a | n/a |

No paid feeds in v1. Multi-venue live in M2.

---

## Performance targets

| Metric | Target | Validated at |
| --- | --- | --- |
| Ingestion throughput (single venue) | ≥ 500 ticks/s peak | 575-694/s in smoke (#9, #10) |
| Engine per-bar latency | < 1 s wall-clock | M1 #20 benchmark |
| Engine determinism | bit-for-bit identical across runs | M1 #21 (`cargo test`) |
| Engine vs Python reference | `|Δσ²_30d| < 1e-6` | M1 #21 acceptance gate |
| Public API p95 latency (REST `latest`) | < 50 ms | M1 #22-24 |
| Public WS broadcast fan-out | 100 concurrent subs / instance | M2 sizing |
| Index publish miss rate | < 0.1 % of expected 60s slots | M2 SLO |

---

## Project status

VolX is in active development.

| Phase | Window | Deliverable | State |
| --- | --- | --- | --- |
| **M0** Research | Done | Python reference impl, validated math, DVOL gap diagnosed | **Complete** |
| **M1** Local pipeline | In progress | Rust ingest + engine → Go API → Next.js | Ingestion + reconnect shipped |
| **M2** Hardening | Pending M1 | Multi-venue, API keys, rate limit, status page, backups | Not started |
| **M3** Public launch | Pending M2 | Methodology page, aggregator listings, public dashboard | Not started |

### What works today

- **`volx-ingestion`** — live Deribit WebSocket connector with REST
  instrument discovery, batched subscribe, ticker → `OptionTick`
  normalisation, reconnect + exponential backoff (1 → 2 → 4 → 8 → 16 s,
  cap 30 s, ±20 % jitter), per-venue task isolation.
- **`volx-shared-types`** — `OptionTick`, `Strip`, `StripQuote`,
  `IndexValue`, `StripHash`, `Years`, `Minutes` and the venue/asset/kind
  enums. All serde-round-trip-tested; domain invariants enforced at
  deserialize time.
- **Python reference impl** — fitted-IV variant adopted as canonical;
  matches DVOL within 5.83 % median absolute relative error (the +5.77 %
  bias is a structural inverse-contract artefact, not a math error — see
  METHODOLOGY §6 and issue #39).

### What's next

- `normalizer` filters (#12) + ClickHouse writer (#15, #16)
- `engine` strip builder (#17), variance integral (#18), 30-day interp
  (#19), scheduler (#20)
- Go API skeleton (#22) → endpoints (#23) → WebSocket stream (#24)
- Next.js scaffold (#25) → landing (#26) → live chart (#27)
- CI (#28)

---

## Roadmap

High-level landmarks:

```
M0 ✓  Research + math reference + DVOL diagnosis
M1    Local live pipeline
       ├── Rust workspace skeleton (#7)              ✓
       ├── shared-types (#8)                         ✓
       ├── Ingestion: Deribit WS (#9)                ✓
       ├── Reconnect + backoff (#10)                 ▶  (this milestone)
       ├── Tracing + Prometheus (#11)
       ├── Normalizer filters + ClickHouse (#12-16)
       ├── Engine: strip / variance / interp / cron (#17-20)
       ├── Engine numerical acceptance (#21)
       ├── Go API: REST + WS (#22-24)
       └── Next.js dashboard (#25-27)
M2    Multi-venue, API keys, status page, backups, SLO monitoring
M3    Methodology page, aggregator submissions, public launch
```

---

## Comparison to alternatives

| Dimension | **VolX** | Deribit DVOL | Volmex BVIV / EVIV |
| --- | --- | --- | --- |
| Operator | Open-source / self-hosted | Deribit (exchange) | Volmex Finance |
| Live since | 2026 (in validation) | 2021 | 2022 |
| Assets | BTC + ETH | BTC | BTC, ETH, multi-asset (MVIV) |
| Tenor | 30-day | 30-day | Full term structure (1D / 7D / 14D / 30D / 60D / 90D / 180D) |
| Number of venues | **3** | 1 | 2 |
| Venues used | Deribit, OKX, Bybit | Deribit | Deribit, OKX |
| Blending method | **Per-venue strip + median** | n/a (single venue) | Aggregated quote universe |
| Outlier rejection | **5 % deviation · 5-tick streak · quorum rollback** | n/a | None published (cannot drop with only 2 venues) |
| Per-tick confidence score | **Yes (venue × freshness × strikes)** | — | — |
| Update frequency | 60 s | 1 s | 1 s |
| Methodology base | Cboe VIX (2003) | Cboe VIX (2003) | Cboe VIX (2003) |
| Methodology published | **Full spec + every deviation documented** | Yes (whitepaper) | Yes (overview) |
| Source code public | **Yes — permissive licence** | No | No |
| Self-hostable | **Yes — `docker compose up`** | No | No |
| Data feed cost | **$0 (public exchange WS)** | $0 (Deribit public REST/WS) | Free public tier · paid institutional feed |
| Determinism guarantee | **Bit-for-bit (`|Δσ²_30d| < 1e-6`)** | Implicit | Unknown |
| Trading product (perp futures) | n/a (research / reference) | Trades on Deribit itself | BVIV / EVIV perps on Bitfinex, gTrade, Polymarket |
| Best-known strength | **Robustness · transparency · self-hostability** | Liquidity-weighted single-venue truth · simplest methodology | Multi-tenor term structure · tradable perpetuals |
| Best-known weakness | Newer · not battle-tested · 30-day validation still pending | Single point of failure (only Deribit data) | Cannot outlier-drop with only 2 venues · methodology not fully public |

VolX is positioned as the **reproducible, auditable, free baseline** that
crypto-native projects, researchers, and risk teams can wire into any
workflow without licensing or SLA negotiation.

---

## Reference implementations

The methodology was validated on real data before any Rust was written:

| Notebook | What it proves |
| --- | --- |
| `research/vix-spx-replication.ipynb` | CBOE math reproduced on synthetic SPX data within published tolerance. |
| `research/bvol-backtest.ipynb` | End-to-end run on Tardis-fetched Deribit chain snapshots: forward solve, strip build, variance integral, 30-day interpolation. All math preconditions satisfied across 144 hourly snapshots. |
| `research/bvol-dvol-gap-diagnostics.ipynb` | Diagnoses the structural +5.77 % gap to Deribit DVOL as an inverse-contract artefact (not a math bug), and adopts fitted-IV smoothing as the canonical methodology — the 1.3-percentage-point improvement over listed-strike CBOE that brought BVOL inside ±6 % of DVOL on every snapshot. |

The Rust engine must numerically match the fitted-IV variant of these
notebooks within `1e-6` in `σ²_30d` to ship.

---

## Security

VolX is read-only software at v1: no on-chain components, no wallet code,
no user funds custody, no auth-token issuance. The threat surface is:

- **Ingestion auth keys** (M2 onwards, for `.raw` channels). Stored in
  Keychain / Vault / k8s secrets, never in env files or repo.
- **API key issuance** (M2). Hashed at rest; rate-limited and per-key
  audit logged.
- **Public dashboard** (M3). Cloudflare WAF + Caddy TLS; no PII
  collected.

To report a vulnerability privately: open a security advisory on GitHub
(`obchain/volx` → Security → Report a vulnerability) rather than a public
issue.
