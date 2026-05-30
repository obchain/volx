# VolX On-Chain Perp — Sepolia Demo

> **Testnet demo. Not audited. No real value.** Ethereum Sepolia (chainId `11155111`).
> This is the tradeable layer on top of the VolX index — a gTrade-style synthetic
> volatility perp where one shared LP vault is the counterparty to every trade.
> There is **no funding rate** in v1.

## What it is

VolX publishes an open BVOL/EVOL volatility index (see [METHODOLOGY](../METHODOLOGY.md)).
This layer makes it **tradeable**: an off-chain keeper pushes the live index on-chain,
and traders open leveraged long/short bets that settle against an LP collateral vault.

```
[VolX Go API] --keeper(deviation+heartbeat)--> [VolXOracle] <--reads-- [VolXPerp vault]
   BVOL/EVOL live                               on-chain price        LP pot + positions
```

- **Loss capped at collateral; a winning long's gain capped at notional** — so the vault
  stays solvent against its reserved notional by construction.
- **80%** liquidation threshold, **0.1%** open/close fee, **10×** max leverage, oracle
  scale **1e8**, collateral **MockUSDC (6 decimals)**.

## Deployed contracts (verified on Etherscan)

| Contract | Address |
|---|---|
| MockUSDC | [`0x60137f8457Db371EE4092c5F6C8e389168C582F5`](https://sepolia.etherscan.io/address/0x60137f8457Db371EE4092c5F6C8e389168C582F5) |
| VolXOracle | [`0x1762841A53F396B6C55eFbbB662D17A3B7Fa4947`](https://sepolia.etherscan.io/address/0x1762841A53F396B6C55eFbbB662D17A3B7Fa4947) |
| VolXPerp (vault) | [`0x1BE8387f05d3556002683Fe0DE9131B15002b7fb`](https://sepolia.etherscan.io/address/0x1BE8387f05d3556002683Fe0DE9131B15002b7fb) |

Canonical source of truth: [`contracts/deployments/sepolia.json`](../contracts/deployments/sepolia.json)
(the keeper and frontend read it). The vault is seeded with 200,000 mUSDC of demo liquidity;
the deployer is the oracle keeper.

## Run it

### 1. Keeper (push the live index on-chain)

```bash
cd keeper
pnpm install
set -a; . ../.secrets/sepolia.env; set +a   # SEPOLIA_RPC_URL + PRIVATE_KEY (deployer/keeper)
VOLX_API_URL=http://localhost:8090 pnpm start
```

The keeper polls the VolX REST API and writes `VolXOracle.updateBoth` only on a **0.5%
deviation** or **30-minute heartbeat**, batching both indices into one tx. `DRY_RUN=1`
logs without sending; `RUN_ONCE=1` runs a single cycle (handy for cron). See
[`keeper/README.md`](../keeper/README.md).

> The on-chain price has a **1-hour staleness guard** (`getPriceChecked`). If the keeper
> is offline, `openPosition` reverts `StalePrice` and the UI disables opening — restart
> the keeper to refresh.

### 2. Frontend (trade + provide liquidity)

```bash
cd frontend
pnpm install
pnpm dev   # http://localhost:3000
```

- **/trade** — connect a wallet (Sepolia), claim mUSDC from the faucet, open/close
  leveraged long/short on BVOL/EVOL, watch live PnL.
- **/pool** — deposit/withdraw LP liquidity; see TVL, utilization, and share price.

### 3. Deploy from scratch (optional)

```bash
cd contracts
forge script script/Deploy.s.sol --rpc-url "$SEPOLIA_RPC_URL" \
  --broadcast --verify --etherscan-api-key "$ETHERSCAN_API_KEY"
```

Writes `deployments/sepolia.json`. `SEED_USDC` (whole tokens, default 200,000) overrides
the LP seed. See [issue/PR history] for the original deploy run (~0.0087 ETH).

## End-to-end run (executed live on Sepolia, 2026-05-30)

A full vertical — keeper push → open long + short → price move → profitable close →
liquidation → LP fee yield — run against the deployed contracts:

| Step | Tx | Result |
|---|---|---|
| Keeper pushes live index | [`0x66f2ab45…`](https://sepolia.etherscan.io/tx/0x66f2ab45a275589961722c4d524cfdb050a9a6c358d4a7e790e4989a8f82bf9f) | BVOL 36.57, EVOL 50.37 on-chain |
| Open **long** BVOL 1,000 mUSDC 5× | [`0x893fbf48…`](https://sepolia.etherscan.io/tx/0x893fbf48b9c22edc564f4ad2c5babf8ef41bb83c6620cb120857243dd0fa1aec) | position #0, equity 995 |
| Open **short** BVOL 500 mUSDC 10× | [`0x122cd130…`](https://sepolia.etherscan.io/tx/0x122cd130cf83e553e8f384e03857934cf6e7049f95d178e99f55703c5bce68f6) | position #1, equity 495 |
| Price move 36.5745 → 40.0000 | [`0x89a9a3f3…`](https://sepolia.etherscan.io/tx/0x89a9a3f34df08a6704a53619d9db5db0a92ac6848a77083d8b93bc77b142e2eb) | long **+465.95**, short **−463.61** (liquidatable) |
| Close long (realize profit) | [`0xfad3af42…`](https://sepolia.etherscan.io/tx/0xfad3af429bc4e53e94887a09c4cbf8e044aef34c27ea3f1d1b2f2c0608ba66a7) | payout **1,455.97 mUSDC** from the vault |
| Liquidate short | [`0xe64a49a5…`](https://sepolia.etherscan.io/tx/0xe64a49a5afaa4bf93a81f85735e2171667e8c06e892bb639c4f766155fddb91a) | liquidator reward **+4.95** (1%), remainder to vault |

> Figures are read straight from on-chain `positionValue` / token balances. Index
> values display to 2dp, but PnL settles on the **exact 1e8 oracle values** — entry
> `3657450885` (36.5745089), mark `4000000000` (40.0) — so e.g. long PnL =
> `working 995 × 5 × (40.0 − 36.5745089) / 36.5745089 = +465.95`.

**LP fee yield:** vault TVL went **200,000.00 → 200,039.08 mUSDC** across the cycle —
open/close fees plus the liquidated short's loss, net of the long's payout — so the LP pot
grew while acting as counterparty. Reserved notional returned to 0 after both positions closed.

## Gas notes

- Deploy (3 contracts + seed): **~0.0087 ETH** one-time.
- Keeper `updateBoth` (both indices, one tx): **~0.00008 ETH** per push. Deviation (0.5%) +
  heartbeat (30m) batching stretches a small ETH balance over days; pause the keeper when
  not demoing.
- A full open/close/liquidate trade cycle: **~0.003 ETH** at ~5 gwei. Sepolia gas is
  volatile (seen 1.6 → 21 gwei within an hour), so budget headroom.

## Disclaimers

Testnet only, **not audited**, no funding rate, demo liquidity. MockUSDC is freely mintable
test money with no value. The oracle value is authoritative from the off-chain VolX engine —
there is no on-chain VIX math. Do not deploy this to mainnet or treat it as production-grade.
