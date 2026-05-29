# VolX Contracts (M4 — on-chain volatility perp)

Solidity layer that makes the VolX index tradeable: a gTrade-style synthetic
perp where one shared LP vault is counterparty to all long/short bets on
BVOL/EVOL, settled against the index pushed on-chain by a keeper.

**Scope:** testnet demo only — Ethereum Sepolia (chainId `11155111`). Not
audited, not mainnet.

## Layout

```
contracts/
  src/        product contracts (MockUSDC, VolXOracle, VolXPerp) — land per issue
  test/       forge tests
  script/     deploy scripts
  lib/        git-submodule deps (forge-std, openzeppelin-contracts)
```

Deps are pinned git submodules — `forge-std@v1.9.4`,
`openzeppelin-contracts@v5.1.0`. Resolved via `remappings.txt`.

## Build order (M4 issues)

`#85 scaffold` → `#86 MockUSDC` → `#87 VolXOracle` → `#88 vault` →
`#89 open/close+PnL` → `#90 liquidation+fees` → `#91 tests` →
`#92 deploy/verify` → `#93 keeper` → `#94 frontend` → `#95 e2e demo`.

## Demo defaults (locked)

max leverage **10x** · liquidation at **80%** collateral loss · **0.1%**
open/close fee · **no funding rate** (v1) · oracle scale **1e8** ·
MockUSDC **6 decimals**.

## Local dev

```bash
# from contracts/
forge build           # compile
forge test            # run suite
forge fmt --check     # formatting gate (CI enforces)
```

Clone with submodules (`git submodule update --init --recursive`) or run
`forge install` if `lib/` is empty.
