# Changelog

All notable changes to VolX are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versioning is
[SemVer](https://semver.org/). Pre-`1.0.0` is a testnet demo — APIs, contracts,
and the index spec may change.

## [0.1.0] — 2026-05-31

First public release: a working, exchange-neutral crypto volatility index
(BVOL/EVOL) plus a live on-chain volatility perp on Ethereum Sepolia.

### Index engine (Phases 0–2)

- **VIX-style variance replication** for BTC and ETH from live options order
  books, producing the **BVOL** and **EVOL** 30-day implied-volatility indices.
- **Reference implementation** validated against the CBOE VIX spec on historical
  SPX data; deterministic, reproducible math (see [METHODOLOGY](METHODOLOGY.md)).
- **Live pipeline** — Rust ingestion → Rust engine → Go API → ClickHouse + Redis,
  streaming indices over REST + WebSocket.
- **Multi-venue robustness** and a per-tick **confidence score** that downweights
  stale/illiquid strikes.

### On-chain perp (Phase 4)

- **VolXOracle** — keeper-pushed on-chain BVOL/EVOL price feed (packed
  value/timestamp/confidence, 1-hour staleness guard).
- **VolXPerpV2** — gTrade-style synthetic volatility perp where a single
  ERC4626-style LP vault is the counterparty to every trade:
  - Leveraged long/short on BVOL/EVOL up to **10×**, **80%** liquidation
    threshold, **0.1%** open/close fee.
  - **Funding** — continuous borrow fee on open notional accrues to the vault
    (owner-tunable, default 0.3%/day); folds into equity.
  - **Conditional orders** — limit-open, take-profit, and stop-loss, executed
    permissionlessly by the keeper when the oracle crosses the trigger.
  - Loss capped at collateral; winning-long gain capped at notional — vault
    solvent against its reserve by construction.
- **Keeper** (TypeScript + viem) — pushes the live index on a 0.5% deviation /
  30-minute heartbeat and sweeps/executes triggered conditional orders.
- **Frontend** (Next.js + viem) — `/trade` (position-aware chart, market/limit
  orders, TP/SL, live PnL + funding), `/pool` (LP deposit/withdraw), and a
  `/dashboard` (TVL, open interest, utilization, long/short skew).
  Deployed on Netlify.
- **End-to-end run executed live on Sepolia** — keeper push → open long + short →
  price move → profitable close → liquidation → LP fee yield. See
  [docs/onchain-demo.md](docs/onchain-demo.md).

### Deployed contracts (Sepolia, verified)

| Contract | Address |
|---|---|
| MockUSDC | `0x60137f8457Db371EE4092c5F6C8e389168C582F5` |
| VolXOracle | `0x1762841A53F396B6C55eFbbB662D17A3B7Fa4947` |
| VolXPerpV2 | `0xc2f0dD6fCaCC29BB90D24dCF16bf95bc7D08BCBB` |

### Notes

- **Testnet only, not audited, no real value.** MockUSDC is freely mintable test
  money. Do not deploy to mainnet or treat as production-grade.

[0.1.0]: https://github.com/obchain/volx/releases/tag/v0.1.0
