# VolX BVOL — Methodology

**Version:** 0.1.0 (M0 draft)
**Status:** Reference specification for the M1 Rust engine. Public-facing
documentation will derive from this file.

This document specifies how the **BVOL** index is computed from option chain
data. It is the canonical contract between the production Rust engine
(crates `engine`, `normalizer`) and the Python reference implementations
that validated the methodology:

- `docs/vix-notes.md` — derivation of the underlying CBOE variance-swap
  formula (the math reference).
- `research/vix-spx-replication.ipynb` — validated SPX implementation
  (the synthetic-data sanity check).
- `research/bvol-backtest.ipynb` — end-to-end run on real Deribit chains.
- `research/bvol-dvol-gap-diagnostics.ipynb` — fitted-IV refinement adopted
  here and DVOL-gap diagnosis.

This file specifies the **VolX-specific adaptations** to crypto options and
the operational details the engine must honour.

---

## 1. What BVOL is

BVOL is a 30-day constant-maturity volatility index for crypto assets,
computed from the implied volatilities of listed BTC and ETH options. It is
the crypto analogue of the CBOE VIX:

- One number per asset (currently BTC and ETH), recomputed every 60 seconds.
- Annualized, expressed in volatility points (e.g. `BVOL = 65.4` means 65.4%
  expected annualized vol of the underlying over the next 30 days).
- Methodology: **CBOE-style variance-swap replication** (Carr-Madan) over a
  smooth implied-volatility surface fitted to listed Deribit options.

BVOL is **not** a clone of Deribit's DVOL. See §6 for the relationship.

---

## 2. Inputs

| Field                  | Source                                  | Frequency       |
| ---                    | ---                                     | ---             |
| Option chain (live)    | Deribit WebSocket `book.{instrument}`   | tick (sub-second) |
| Underlying spot / index | Deribit `deribit_price_index.{btc,eth}_usd` | tick           |
| Risk-free rate `r`     | constant 0 (see §4.4)                   | -               |

The historical reference uses Tardis options-chain parquets for the same
schema. See `research/scripts/tardis_fetch.py`.

Per option, the engine consumes:

- `strike_price` (USD)
- `expiration_ts` (UTC)
- `option_type` (call / put)
- `bid_price`, `ask_price`, `mark_price` (quoted in coin — BTC or ETH)
- `mark_iv` (Deribit-published implied volatility, percent)
- `underlying_price` (USD spot at quote time)

### 2.1 Quote-to-USD conversion

Deribit options are quoted in coin. To put quotes on a common (USD) basis
the engine multiplies by the contemporaneous `underlying_price`:

```
price_usd = price_coin × underlying_price
```

This conversion is **not** an exact change-of-numeraire — see §6. It is the
convention the rest of the pipeline uses.

---

## 3. Quote filters

Two filter layers — keep them in distinct code paths.

### 3.1 Normalizer layer (pre-snapshot, applied per tick in live mode)

Operates on each `(instrument, side)` quote as ticks arrive. A side that
fails any rule is treated as missing for the next downstream snapshot.

| Filter                  | Rule                                                      |
| ---                     | ---                                                       |
| Staleness               | Drop quote if last tick > 5 s old.                        |
| Crossed / locked        | Drop if `ask_price ≤ bid_price`.                          |
| Spread                  | Drop if `(ask - bid) / mid > 0.30` (30 % spread).         |
| Below intrinsic         | Drop if mid < intrinsic value (numerical tolerance 1e-9). |

These filters do **not** apply in the historical Tardis pipeline — that
input is already snapshot data with no real-time concerns.

### 3.2 Engine layer (per-snapshot, applied at compute time)

Operates on the (already-normalized) snapshot before the IV surface fit.

| Filter                  | Rule                                                      |
| ---                     | ---                                                       |
| Missing IV              | Drop strike if `mark_iv ≤ 0.001` or `mark_iv` not finite. |
| Missing side            | Drop strike if either the call or the put leg is missing for that `(strike, expiry)`. |
| Strip min size          | Reject the expiry if fewer than 5 strikes survive the two filters above. |

The CBOE-baseline "zero-bid wing-termination" rule is **not part of the
canonical engine pipeline**; it belonged to the listed-strike CBOE variant
which we no longer use (replaced by fitted-IV smoothing in §4.3). The rule
remains documented in `docs/vix-notes.md` for completeness only.

---

## 4. Index computation

Given a synchronized snapshot of all quotes at timestamp `t`:

### 4.1 Expiry selection

Pick two listed expiries that bracket 30 days:

- **Near:** the **largest** listed expiry with time-to-expiry in
  `[7 days, 30 days)`. We deliberately use `max(near)` instead of CBOE-strict
  `min(near)` because it tightens the bracket around the 30-day target and
  reduces extrapolation error in §4.6. See `research/bvol-backtest.ipynb`
  `select_near_next` for justification.
- **Next:** the **smallest** listed expiry with time-to-expiry `> 30 days`.

If no pair satisfies both constraints, the index is **not published** for
that snapshot — the engine emits a "no expiry pair" status instead.

### 4.2 Forward price (per expiry)

Put-call parity, restricted to strikes with both sides quoted above `1e-9` USD:

```
K* = argmin_K |C_usd(K) − P_usd(K)|   subject to both sides quoted > 1e-9 USD
F  = K* + e^{rT} · (C_usd(K*) − P_usd(K*))
```

If no strike has both sides quoted above the threshold, the expiry is
rejected.

(Tie-break: smallest strike index, per `np.argmin`.)

### 4.3 IV surface (per expiry)

The CBOE white paper uses listed-strike option prices directly. On BTC's
coarse strike grid (\$1 000–5 000 spacing) the Riemann discretisation error
is material. The VolX-specific adaptation is **fitted-IV smoothing**:

1. Collect `mark_iv` at every listed strike. Use the call-side IV as
   primary; if the call side is missing for that strike, fall back to the
   put-side IV. (Deribit publishes a single IV per `(strike, expiry)` so
   the two are equal where both sides quote.)
2. Filter out strikes with missing or non-finite IVs (§3.2).
3. Fit a **natural cubic spline** in log-moneyness `x = ln(K / F)` with no
   extrapolation outside the listed strike range.
4. Sample a dense strike grid of **801 points** linearly between
   `K_min` and `K_max` of the listed strikes.
5. Evaluate the spline on the dense grid → `iv_dense(K)`. Reject the
   snapshot if any sampled IV is NaN (spline domain error).
6. **Clamp the result:** `iv_dense ← clip(iv_dense, 1e-4, 5.0)`. This caps
   near-zero IVs (which would produce divergent BS prices) and unphysical
   500 %+ IVs (which would dominate the integral). The clamp is part of the
   canonical methodology — a Rust port that skips it will produce different
   values for snapshots whose spline produces tiny or huge IVs.

The grid size, the natural-spline boundary condition, the
no-extrapolation choice, and the clamp bounds are non-negotiable;
changing any of them changes the index.

### 4.4 Risk-free rate

`r = 0`. Rationale:

- Deribit DVOL uses `r = 0` (DVOL whitepaper) so this matches the most
  natural benchmark.
- For 30-day horizons in current rate environments the `e^{rT}` factor is
  within 0.5 % of 1, smaller than other error sources.
- Removing rate input also removes a parameter that would otherwise need a
  per-currency, time-varying source.

A future version may take USDC or USDT lending rates as `r`; doing so will
be a methodology version bump (§9).

### 4.5 Per-expiry variance

K₀ = the largest point on the **dense grid** at or below F. Compute Carr-Madan
OTM prices on the dense grid using the fitted IVs:

```
P_dense(K) = BS_put(F, K, T, r, iv_dense(K))     for K < F
C_dense(K) = BS_call(F, K, T, r, iv_dense(K))    for K > F
Q(K) = P_dense(K)                                for K < F
Q(K) = C_dense(K)                                for K > F
Q(K₀) = (P_dense(K₀) + C_dense(K₀)) / 2          at the split point
```

Trapezoidal integration of the Carr-Madan integrand:

```
σ²_T = (2 e^{rT} / T) · ∫ Q(K) / K² dK   −   (F / K₀ − 1)² / T
```

The integral is taken over the dense grid (`[K_min, K_max]`). Strikes beyond
listed range are **not** extrapolated — extending the IV surface past the
listed wings was shown empirically to *increase* error (see
`bvol-dvol-gap-diagnostics.ipynb` §4 and the rejected `wing_extend` variant
in the diagnostic harness).

Reject the snapshot if `σ²_T < 0`. This should not happen with a healthy
spline fit; if it does, the upstream data is bad.

### 4.6 30-day interpolation

In total-variance space, **not** in volatility, **not** in annualized
variance. Use minutes throughout to avoid fractional-year rounding:

```
N₃₀  = 30 · 1440
N₃₆₅ = 365 · 1440
T₁   = N_T₁ / N₃₆₅            # time to near expiry, years
T₂   = N_T₂ / N₃₆₅            # time to next expiry, years
w₁ = (N_T₂ − N₃₀) / (N_T₂ − N_T₁)
w₂ = (N₃₀  − N_T₁) / (N_T₂ − N_T₁)
σ²_30d = (T₁ · σ²₁ · w₁ + T₂ · σ²₂ · w₂) · (N₃₆₅ / N₃₀)
```

Reject the snapshot if `σ²_30d < 0`.

### 4.7 Index value

```
BVOL = 100 · √σ²_30d
```

Published in volatility points (e.g. `BVOL = 65.40`).

---

## 5. Operational requirements

These are the M1 engine's contract with consumers.

| Requirement              | Value                                                        |
| ---                      | ---                                                          |
| Recompute cadence        | 60 s scheduled; every snapshot is a fresh full computation.  |
| Publish cadence          | Same as recompute (one row per 60 s into ClickHouse).        |
| Timestamp convention     | UTC, ISO 8601, millisecond precision. The published time is the snapshot timestamp (start of the bar), NOT the wall-clock when the engine finished. |
| Failure semantics        | If any §4 step rejects the snapshot, publish a null row with `status` indicating the reason; do **not** carry forward the previous value. |
| Per-expiry strip minimum | After IV filtering, require ≥ 5 valid strikes; otherwise reject the expiry and (if either near or next fails) the snapshot. |
| Numerical precision      | All intermediate quantities in `f64`. No `f32` shortcuts.    |
| Determinism              | Same inputs → same `f64` output, bit-for-bit, across engine instances. |

---

## 6. Relationship to Deribit DVOL

BVOL **is not** a clone of Deribit DVOL. They differ by a documented and
expected margin.

| Property                   | Our BVOL                                | Deribit DVOL                          |
| ---                        | ---                                     | ---                                   |
| Contract spec assumption   | Vanilla, USD-settled                    | Inverse, BTC-settled                  |
| Variance integral kernel   | `1/K²` (Carr-Madan, vanilla derivation) | Deribit's published variant           |
| IV source                  | Fitted spline of `mark_iv`              | Deribit-internal smoothing            |
| Strike grid                | Dense (801 points)                      | Deribit-internal                      |
| Risk-free rate             | 0                                       | 0                                     |

On the M0 backtest dataset (3 first-of-month days × 2 currencies × 24 hours
= 144 hourly snapshots, `research/bvol-dvol-gap-diagnostics.ipynb`,
canonical fitted-IV variant):

- **Median absolute relative error**: 5.83 %.
- **p95 absolute relative error**: 8.66 %.
- **Bias**: BVOL is **+5.77 % higher** than DVOL on average.
- **Time-series correlation** (from the baseline-listed-strike PR #36 run;
  not re-computed for the fitted-IV variant but expected to be at least as
  good given the lower residual error): 0.955 (BTC), 0.981 (ETH).

The bias is **not a math error**. Per `research/bvol-dvol-gap-diagnostics.ipynb`
§5, the residual is structural: Deribit BTC + ETH options pay
`(K − S_T) / S_T` in BTC (the **inverse** spec), not `(K − S_T)` in USD.
Their published `mark_iv` is calibrated under the BTC-numeraire. Feeding
USD-converted quotes into the vanilla-CBOE integral inherits a small
multiplicative offset (per-option ratio 1.03–1.07 across ATM and near-OTM
strikes, with occasional deep-OTM outliers above 1.15 driven by Deribit's
sub-0.001 BTC tick-size granularity) that gets amplified through the
integral.

Future work (issue #39) will derive a DVOL-aligned variant by re-deriving
Carr-Madan replication for inverse contracts. That work is **not** required
to ship BVOL.

---

## 7. Reference implementations

| Artifact                                         | Status                                            |
| ---                                              | ---                                               |
| `research/vix-spx-replication.ipynb`             | CBOE math validated on synthetic SPX (issue #3, PR #30 merged) |
| `research/bvol-backtest.ipynb`                   | End-to-end run on real Deribit chains (issue #5, PR #36 merged) |
| `research/bvol-dvol-gap-diagnostics.ipynb`       | DVOL-gap diagnosis + fitted-IV adoption (issue #37, PR #38 merged) |
| `crates/engine`                                  | M1 Rust port (issues #17–#20, in progress)        |

The Rust engine **must** numerically match the fitted-IV variant of
`research/bvol-dvol-gap-diagnostics.ipynb` on the same input dataset to
within 1e-6 in `σ²_30d` (≈ 1 part per million in BVOL). This is the M1
acceptance criterion for engine correctness (#21).

---

## 8. Known limitations

1. **Single venue.** BVOL is currently sourced from Deribit alone. Multi-venue
   blending (PRD §10) is deferred to a post-launch milestone — Deribit is
   the dominant BTC + ETH options venue and a single-venue index is
   defensible at launch.
2. **No deep-wing extrapolation.** The dense grid spans only `[K_min, K_max]`
   of the listed strikes. Empirically, extending past listed wings (whether
   via flat IV or any other extrapolation we tried) made the DVOL gap worse,
   so the simpler interior-only integral is canonical.
3. **Inverse-contract artefact.** The +5–6 % offset to DVOL is structural,
   not a bug — see §6 and issue #39.
4. **Settlement-time mark conventions.** Deribit publishes DVOL hourly OHLC
   candles timestamped at the candle open. When backtesting BVOL vs DVOL,
   note that `dvol.close[t]` is the end-of-hour value for the candle opening
   at `t`, i.e. a 1 h forward window from our snapshot. This is a benchmark
   artefact, not an index artefact.

---

## 9. Versioning

This methodology is `0.1.0`. Versions follow semver. Any change that
affects published values bumps the version:

| Change kind                                                  | Next version from `0.1.0` |
| ---                                                          | ---                       |
| Bug fix that does not change any published value             | `0.1.1`                   |
| Parameter change (grid size, near-rule, `r`, filters)        | `0.2.0`                   |
| Algorithm change (different integral, different smoothing)   | `1.0.0`                   |

Historical values are **never rewritten** on a methodology bump. The change
log lives in §10; old engine binaries retain backward semantics via the
version field on the published row.

---

## 10. Change log

| Version | Date       | Change                                                |
| ---     | ---        | ---                                                   |
| `0.1.0` | 2026-05-21 | Initial draft (M0 #6). Fitted-IV smoothing canonical. |

---

## 11. References

- CBOE Global Markets. *White Paper: CBOE Volatility Index* (revised 2019).
- Carr, P., Madan, D. *Towards a Theory of Volatility Trading* (1998).
- Jiang, G., Tian, Y. *The Model-Free Implied Volatility and Its Information
  Content* (2005).
- Deribit. *DVOL Methodology* (public docs).

All three CBOE-lineage references are public and have been the basis of
model-free implied volatility for over two decades. No proprietary IP is
reproduced here.
