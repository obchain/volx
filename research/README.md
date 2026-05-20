# research/

Python sandbox for VolX. Purpose:

1. **Reference implementations** of the index math (truth oracle for Rust engine tests).
2. **Backtests** against external implied-volatility benchmarks.
3. **Methodology validation** before any number is shipped to production.

The notebooks here are NOT production code. They exist so the math can be verified
end-to-end in a language with mature numerical libraries before it is ported to Rust.

---

## Setup

Requires Python 3.11+.

```bash
cd research
python -m venv .venv
source .venv/bin/activate          # macOS / Linux
# .venv\Scripts\activate           # Windows
pip install -r requirements.txt
jupyter lab
```

`jupyter lab` opens at <http://localhost:8888>.

---

## Notebooks

| File | Purpose | Pairs with issue |
|---|---|---|
| `vix-spx-replication.ipynb` | Reproduce published VIX from SPX option chains, target ±0.5 pts | #3 |
| `bvol-backtest.ipynb` | Run the VIX formula on Deribit BTC options, compare vs DVOL | #5 |
| `methodology-validation.ipynb` | Numerical comparison of Rust engine output vs Python reference | M1 |

---

## Data

Pulled / generated locally under `research/data/`. Not committed (see `.gitignore`).

Sources:

- **SPX options** — CBOE historical (manual download).
- **BTC + ETH options** — Deribit public REST (`/public/get_book_summary_by_currency`, `/public/get_instruments`).
- **DVOL** — Deribit public history endpoint.

Loaders live inline in each notebook for now. Promote to `research/lib/` once shared
across more than one notebook.

---

## Conventions

- Cells should be reproducible top-to-bottom; no hidden state from out-of-order runs.
- Pin random seeds where used.
- Cache pulled data to Parquet under `research/data/<source>/` so reruns are cheap.
- Keep math symbols matching `docs/vix-notes.md` / `METHODOLOGY.md` so cross-referencing is trivial.
