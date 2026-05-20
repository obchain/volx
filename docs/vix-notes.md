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
| $\sigma^2$ | Annualized variance for expiry $T$. |
| $\sigma$ | VIX = $100 \cdot \sqrt{\sigma^2_{30\text{d}}}$. |

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

Walking outward from $K_0$ on each side, **stop as soon as two consecutive strikes have a zero bid**. Discard those two strikes and every strike beyond them. The intent: deep wings are illiquid and noisy; if liquidity dies, the strip ends.

Result: a finite, contiguous set of strikes $\{K_1, \ldots, K_n\}$ centered around $K_0$.

---

## 5. Variance Integral (Carr-Madan Replication)

For a single expiry $T$:

$$
\sigma^2 = \frac{2}{T} \sum_i \frac{\Delta K_i}{K_i^2} \, e^{rT} \, Q(K_i) \;-\; \frac{1}{T} \left( \frac{F}{K_0} - 1 \right)^2
$$

**Reading the formula:**

- $\dfrac{\Delta K_i}{K_i^2}$ is the Carr-Madan weight — heavier on near-the-money strikes (small $K_i$) and lighter on the wings.
- $e^{rT} Q(K_i)$ is the present-valued (well, future-valued) option premium at strike $K_i$.
- The first sum is a Riemann approximation to the static-hedge replication of a variance swap.
- The second term is a small correction for the discrete forward $F$ not landing exactly on $K_0$ (the "$K_0$ is the strike at or below $F$, not equal to $F$" issue).

This holds **per expiry**. We need a 30-day constant maturity, which requires interpolation across two expiries.

---

## 6. 30-Day Constant-Maturity Interpolation

Pick the two expiries straddling 30 days:

- **Near term:** the closest expiry $\ge$ 7 days (to avoid microstructure noise near expiry). Time to expiry $T_1$, variance $\sigma_1^2$.
- **Next term:** the next listed expiry after near term. $T_2$, $\sigma_2^2$.

Convert to **minutes** for the interpolation weights:

| Quantity | Value (minutes) |
|---|---|
| $N_{T_1}$ | minutes to near expiry |
| $N_{T_2}$ | minutes to next expiry |
| $N_{30}$ | $30 \cdot 1440 = 43{,}200$ |
| $N_{365}$ | $365 \cdot 1440 = 525{,}600$ |

Interpolate in **total variance space** (not in volatility, not in annualized variance):

$$
\sigma^2_{30\text{d}} \cdot \frac{N_{30}}{N_{365}}
= T_1 \sigma_1^2 \cdot \frac{N_{T_2} - N_{30}}{N_{T_2} - N_{T_1}}
+ T_2 \sigma_2^2 \cdot \frac{N_{30} - N_{T_1}}{N_{T_2} - N_{T_1}}
$$

Solving for the annualized 30-day variance and taking the square root:

$$
\text{VIX} = 100 \cdot \sqrt{\sigma^2_{30\text{d}}}
$$

**Why total variance and not annualized?** Variance is additive in time when returns are independent; volatility is not. Interpolating $\sigma$ directly underweights the longer expiry and biases the result.

---

## 7. Worked Example (Illustrative Numbers)

Setup:

- Underlying spot $S = 4{,}500$, $r = 0.05$, near expiry 23 days ($T_1 = 23/365$), next expiry 37 days ($T_2 = 37/365$).
- Near expiry: smallest $|C - P|$ at $K^* = 4{,}500$ with $C = 60$, $P = 52$.
- $F_1 = 4{,}500 + e^{0.05 \cdot 23/365}(60 - 52) \approx 4{,}508$.
- Strikes listed every 25 pts. $K_0 = 4{,}500$ (largest $\le F_1$).
- OTM strip after wing termination: 30 strikes.
- Apply Section 5 → $\sigma_1^2 = 0.0420$.
- Repeat for next expiry → $\sigma_2^2 = 0.0460$.

Interpolate (Section 6):

$$
\sigma_{30\text{d}}^2 \approx 0.0438
\quad\Rightarrow\quad
\text{VIX} \approx 100 \cdot \sqrt{0.0438} \approx 20.93
$$

The exact arithmetic is in the SPX replication notebook (`research/vix-spx-replication.ipynb`); the numbers above are for orientation only.

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
