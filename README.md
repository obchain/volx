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
                │   ② strip builder (forward via parity, K₀, OTM Q(K)) │
                │   ③ fitted-IV spline + dense-grid resample           │
                │   ④ Carr-Madan variance integral                     │
                │   ⑤ 30-day total-variance interpolation              │
                │   ⑥ publish IndexValue { value, confidence, hash }   │
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
| Ingestion + engine + normalizer | **Rust 1.85** (edition 2024) | `f64` determinism, zero-allocation tick paths, no GC pauses inside the 60 s loop |
| Async runtime | `tokio` + `tokio-tungstenite` (rustls) | mature WS stack; rustls avoids OpenSSL on Oracle Cloud |
| In-process channel | `flume` | bounded MPSC, no `std::sync::mpsc` allocation overhead |
| Storage (ticks + index) | **ClickHouse 24.x** | 15-20× compression on option ticks; sub-second range scans |
| Cache + pubsub | **Redis 7** | hot latest-value reads + WS fanout |
| API | **Go 1.23** + Fiber v3 + gorilla/websocket + prometheus/client_golang | fast HTTP, mature WS, low ops surface |
| Frontend | **Next.js 15** (app router) + React 19 + Tailwind 4 + `lightweight-charts` | static-friendly, deterministic chart rendering |
| Research | Python 3.14 + numpy + pandas + scipy + matplotlib + jupyter | the methodology was validated here before any Rust was written |
| Lint policy | `unsafe_code = forbid`, clippy pedantic, `cargo fmt --check` | financial code; zero tolerance for memory bugs |
| Deploy (M3) | Oracle Cloud Always Free + Cloudflare + Vercel Hobby + GHCR | $0/mo recurring; ~$1/yr domain |

Determinism, free-tier compatibility, and operational simplicity are the
three non-negotiables.

---

## Quickstart

Requires **Rust 1.85+** (edition 2024, resolver 3). No API keys, no
database, no Docker — the ingestion binary runs straight from a fresh
clone.

```bash
# 1. Clone
git clone https://github.com/obchain/volx.git
cd volx

# 2. Live Deribit tick stream (≈500-1000 ticks/s sustained)
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

| Property | **VolX** | Deribit DVOL | T3 / Volmex | Discontinued BVIX |
| --- | --- | --- | --- | --- |
| Methodology public | ✓ (full spec) | partial whitepaper | proprietary | yes |
| Computed from public data | ✓ | ✓ (Deribit-only) | proprietary feeds | CBOE-licensed |
| Self-hostable | ✓ | ✗ | ✗ | ✗ |
| Free historical access | ✓ (planned) | partial (rate-limited) | paid | n/a |
| Determinism guarantee | ✓ (bit-for-bit) | implicit | unknown | n/a |
| Multi-venue blending | M2 | Deribit only | yes (paid) | n/a |
| Cost | $0 | $0 (read) / paid (commercial) | paid | discontinued |

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
