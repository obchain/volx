# CBOE VIX — Distilled Notes

Source: CBOE VIX White Paper (public).
Purpose: minimal, self-contained reference so a reader can reproduce the VIX SPX
replication notebook without going back to the paper.

This document covers ONLY the original VIX (2003 onward) methodology — variance-swap
replication. The older VXO methodology (1993, ATM Black-Scholes IV) is out of scope.

---

## 1. Notation

| Symbol | Meaning |
|---|---|
| $T$ | Time to expiry, in years (CBOE uses minutes / 525,600). |
| $r$ | Risk-free rate to expiry (continuously compounded). |
| $F$ | Forward price of the underlying at expiry $T$. |
| $K_i$ | Strike of option $i$. |
| $K_0$ | First strike at or below the forward. |
| $\Delta K_i$ | Strike interval at $K_i$, $= (K_{i+1} - K_{i-1})/2$ (edges: one-sided). |
| $Q(K_i)$ | Midpoint of bid/ask for the OTM option at $K_i$ (put if $K_i < K_0$, call if $K_i > K_0$, average of put + call if $K_i = K_0$). |
| $\sigma_T^2$ | Annualized variance for expiry $T$. |
| $\sigma_{30\text{d}}^2$ | Annualized 30-day variance (interpolated from two expiries). |
| $\text{VIX}$ | $= 100 \cdot \sqrt{\sigma_{30\text{d}}^2}$ — the published index. |

---

## 2. Forward Price (Put-Call Parity)

For each expiry, find the strike $K^*$ where $|C(K) - P(K)|$ is smallest. Then:

$$
F = K^* + e^{rT} \cdot \big(C(K^*) - P(K^*)\big)
$$

Why: put-call parity gives $C - P = e^{-rT}(F - K)$, so we recover $F$ from the
strike at which the call and put prices are closest.

**Rule of thumb:** call and put are equal in price only when $K = F$. Picking the
strike with the smallest $|C - P|$ is a discrete approximation to that.

---

## 3. $K_0$ Selection

$$
K_0 = \max \{ K_i : K_i \le F \}
$$

The largest listed strike that is still at or below the forward. Defines the split
between the put side and the call side of the OTM strip.

---

## 4. OTM Strip Construction + Zero-Bid Wing Termination

The variance integral is built from **out-of-the-money** options only:

- For $K_i < K_0$: use the **put** at $K_i$.
- For $K_i > K_0$: use the **call** at $K_i$.
- At $K_i = K_0$: use the **average** of put and call midpoints.

### Quality filters (applied before strip construction)

- Drop quotes with zero bid (the wing-termination rule below makes this stricter for the strip edges).
- Drop quotes where mid is below intrinsic value.
- Drop stale quotes (>5s old for the live system; not a concern in historical replication).

### Zero-bid wing termination (CBOE rule)

Walking outward from $K_0$ on each side, **stop as soon as two consecutive strikes have a zero bid**. **Discard both zero-bid strikes and every strike further out** (do not keep the first of the pair). The intent: deep wings are illiquid and noisy; if liquidity dies, the strip ends.

Result: a finite, contiguous set of strikes $\{K_1, \ldots, K_n\}$ centered around $K_0$.

---

## 5. Variance Integral (Carr-Madan Replication)

For a single expiry $T$, CBOE white-paper form (constant $e^{rT}$ factored out of the sum):

$$
\sigma_T^2 \;=\; \frac{2\, e^{rT}}{T} \sum_i \frac{\Delta K_i}{K_i^2} \, Q(K_i) \;-\; \frac{1}{T} \left( \frac{F}{K_0} - 1 \right)^2
$$

At the index $i$ for which $K_i = K_0$, use $Q(K_0) = \tfrac{1}{2}\big(P(K_0) + C(K_0)\big)$ per §4 — the only strike in the strip that uses *both* a put and a call.

**Reading the formula:**

- $\dfrac{\Delta K_i}{K_i^2}$ is the Carr-Madan weight — heavier near the money (small $K_i$), lighter in the wings. Comes from the static-replication weights for a log contract.
- $e^{rT}$ is a single overall factor; it appears because the variance-swap replication is priced at $t=0$ from prices observed at $t=0$, while the variance accrues to $T$. Do NOT interpret $e^{rT} Q(K_i)$ as a "future-valued premium" — $Q(K_i)$ is the observed mid; $e^{rT}$ comes from the parity derivation, not from re-pricing the option.
- The sum is a Riemann approximation to the integral $\int K^{-2} Q(K)\, dK$ that drives the log-contract replication of a variance swap.
- The second term corrects for the fact that $K_0$ is the strike at-or-below $F$, not exactly $F$.

This holds **per expiry**. The published VIX is a 30-day constant maturity, requiring interpolation across two expiries (next section).

---

## 6. 30-Day Constant-Maturity Interpolation

Pick the two expiries straddling 30 days:

- **Near term:** the closest expiry $\ge$ 7 days (to avoid microstructure noise near expiry). Variance $\sigma_1^2$ (annualized, from §5).
- **Next term:** the next listed expiry after near term. Variance $\sigma_2^2$.

Express all times in **minutes** for the interpolation:

| Quantity | Value (minutes) |
|---|---|
| $N_{T_1}$ | minutes to near expiry |
| $N_{T_2}$ | minutes to next expiry |
| $N_{30}$  | $30 \cdot 1440 = 43{,}200$ |
| $N_{365}$ | $365 \cdot 1440 = 525{,}600$ |

Interpolate in **total variance space** (not in volatility, not in annualized variance). With $T_i = N_{T_i} / N_{365}$ (years), and using one consistent unit system throughout:

$$
\sigma^2_{30\text{d}} \cdot \frac{N_{30}}{N_{365}}
= \frac{N_{T_1}}{N_{365}} \sigma_1^2 \cdot \frac{N_{T_2} - N_{30}}{N_{T_2} - N_{T_1}}
+ \frac{N_{T_2}}{N_{365}} \sigma_2^2 \cdot \frac{N_{30} - N_{T_1}}{N_{T_2} - N_{T_1}}
$$

(Multiplying both sides by $N_{365}$ and cancelling gives the more common minute-form $N_{30}\sigma^2_{30\text{d}} = N_{T_1}\sigma_1^2 \cdot w_1 + N_{T_2}\sigma_2^2 \cdot w_2$ with the same time-weights.)

Solving for the annualized 30-day variance and taking the square root:

$$
\text{VIX} = 100 \cdot \sqrt{\sigma^2_{30\text{d}}}
$$

**Why total variance and not annualized?** Variance is additive in time when returns are independent; volatility is not. Interpolating $\sigma$ directly underweights the longer expiry and biases the result.

---

## 7. Worked Example

A real SPX strip carries 50–150 strikes; here we use just 5 strikes so every arithmetic step fits on the page. The mechanics of §5 are identical at any strip size.

### Setup

- Underlying spot $S = 4{,}500$, risk-free $r = 0.05$.
- Near expiry: $T_1 = 23/365 \approx 0.063014$ yr, so $e^{rT_1} \approx 1.003156$.
- Strike grid every 25 pts: $\{4450, 4475, 4500, 4525, 4550\}$.
- Observed mids at near expiry:

| $K$ | Put mid | Call mid |
|---|---|---|
| 4450 | 30 | (deep ITM — ignored) |
| 4475 | 40 | (deep ITM — ignored) |
| 4500 | 52 | 60 |
| 4525 | (deep ITM — ignored) | 48 |
| 4550 | (deep ITM — ignored) | 36 |

### Step 1 — Forward via parity (§2)

$|C - P|$ is smallest at $K^* = 4500$: $|60 - 52| = 8$.

$$
F_1 \;=\; 4500 + e^{0.05 \cdot 0.063014}\!\cdot(60 - 52) \;=\; 4500 + 1.003156 \cdot 8 \;=\; 4508.025
$$

### Step 2 — $K_0$ (§3)

Largest strike $\le F_1 = 4508.025$ is $K_0 = 4500$.

### Step 3 — OTM strip (§4)

| $K_i$ | side | $Q(K_i)$ |
|---|---|---|
| 4450 | Put | 30 |
| 4475 | Put | 40 |
| 4500 | $\tfrac{1}{2}(P + C) = \tfrac{1}{2}(52 + 60)$ | 56 |
| 4525 | Call | 48 |
| 4550 | Call | 36 |

### Step 4 — $\Delta K_i$ (CBOE rule, evenly-spaced grid gives $\Delta K = 25$ everywhere)

- Inner strikes: $\Delta K_i = (K_{i+1} - K_{i-1})/2$. For 4475: $(4500 - 4450)/2 = 25$. Same at 4500 and 4525.
- Edges (one-sided): 4450 → distance to 4475 = 25; 4550 → distance to 4525 = 25.

### Step 5 — Per-strike contribution $\dfrac{\Delta K_i}{K_i^2} Q(K_i)$

| $K_i$ | $\Delta K_i / K_i^2$ | $Q(K_i)$ | term |
|---|---|---|---|
| 4450 | $25 / 4450^2 = 1.2626 \times 10^{-6}$ | 30 | $3.788 \times 10^{-5}$ |
| 4475 | $25 / 4475^2 = 1.2484 \times 10^{-6}$ | 40 | $4.994 \times 10^{-5}$ |
| 4500 | $25 / 4500^2 = 1.2346 \times 10^{-6}$ | 56 | $6.914 \times 10^{-5}$ |
| 4525 | $25 / 4525^2 = 1.2209 \times 10^{-6}$ | 48 | $5.860 \times 10^{-5}$ |
| 4550 | $25 / 4550^2 = 1.2075 \times 10^{-6}$ | 36 | $4.347 \times 10^{-5}$ |
| **sum** | | | $\mathbf{2.5903 \times 10^{-4}}$ |

### Step 6 — Apply leading factor and correction (§5)

Leading factor: $\dfrac{2 e^{rT_1}}{T_1} = \dfrac{2 \cdot 1.003156}{0.063014} = 31.840$.

Sum-term: $31.840 \cdot 2.5903 \times 10^{-4} = 8.247 \times 10^{-3}$.

Correction:
$$
\frac{1}{T_1}\left(\frac{F_1}{K_0} - 1\right)^2
= \frac{1}{0.063014}\left(\frac{4508.025}{4500} - 1\right)^2
= \frac{1}{0.063014} \cdot (1.7833 \times 10^{-3})^2
= 5.046 \times 10^{-5}
$$

Per-expiry variance:
$$
\sigma_1^2 \;=\; 8.247 \times 10^{-3} \;-\; 5.046 \times 10^{-5} \;\approx\; 8.197 \times 10^{-3}
$$

### Step 7 — Interpret

A 5-strike strip cannot capture the wings, so this artificially-truncated $\sigma_1^2 \approx 0.0082$ corresponds to an annualized vol of $\sqrt{0.0082} \approx 9\%$ — much lower than realistic SPX (~20%). Plugging a full 50–150-strike chain into the same arithmetic recovers realistic levels.

For the 30-day interpolation (§6), repeat Steps 1–6 on the *next* expiry to obtain $\sigma_2^2$, then combine. The full reproducible computation lives in `research/vix-spx-replication.ipynb` and matches published VIX to within $\pm 0.5$ pts on 30+ historical days.

---

## 8. Adapting to Crypto (Preview)

The same methodology applies to BTC / ETH options with one substitution:

- **Forward:** in crypto we observe the perpetual or futures basis directly, so $F$ can be sourced from the underlying perpetual mark and cross-checked via put-call parity from the option chain. We prefer parity-derived $F$ from the chain itself for consistency (no perpetual dependency in the index pipeline).
- **Risk-free rate:** for crypto, $r$ is effectively the funding-implied rate. CBOE's choice of US Treasury yields is not directly applicable. Most published crypto vol indices use a small positive constant (e.g. 5% annualized) and treat the correction as negligible — the $e^{rT}$ factor moves slowly. We follow the same convention; sensitivity is documented in the methodology validation notebook.
- **Quote filters:** crypto microstructure is noisier — stale quotes, wider spreads, and zero-bid wings are more common. Filter aggressively before strip construction (PRD §3.2).
- **Multi-venue blend:** unlike SPX which trades on a single regulated venue (CBOE), crypto options are fragmented across Deribit / OKX / Bybit. Blend rule is per PRD §10 / engine spec; out of scope for this notes file.

---

## 9. References

- CBOE Global Markets — *White Paper: CBOE Volatility Index* (revised 2019).
- Carr, P. and Madan, D. — *Towards a Theory of Volatility Trading* (1998).
- Jiang, G. and Tian, Y. — *The Model-Free Implied Volatility and Its Information Content* (2005).

All three are public and have been the basis of model-free implied volatility for over two decades. No proprietary IP is reproduced here.
