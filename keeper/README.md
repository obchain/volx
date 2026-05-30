# VolX Keeper

Off-chain oracle pusher. Polls the VolX REST index (`/v1/index/{bvol,evol}/latest`)
and writes `VolXOracle.updateBoth` on-chain, Chainlink-style: a transaction is
sent only when an index **deviates** past a threshold or the **heartbeat**
elapses. Both indices are batched into a single `updateBoth` tx to halve gas.

## Run

```bash
pnpm install
# config via env (see .env.example); secrets live in repo-root .secrets/sepolia.env
SEPOLIA_RPC_URL=... PRIVATE_KEY=... VOLX_API_URL=https://api.example \
  pnpm start
```

The keeper reads `process.env` directly. The simplest setup sources the same
gitignored secrets the deploy uses:

```bash
set -a; . ../.secrets/sepolia.env; set +a
VOLX_API_URL=http://localhost:8090 pnpm start
```

### Modes

- `DRY_RUN=1` — compute and log the decision, never send a tx.
- `RUN_ONCE=1` — run a single poll/decide/push cycle then exit (used by cron or the e2e test). `pnpm once` is shorthand.

### Config (env)

| Var | Default | Meaning |
|---|---|---|
| `SEPOLIA_RPC_URL` | — (required) | JSON-RPC endpoint |
| `PRIVATE_KEY` | — (required) | keeper signer; must equal `VolXOracle.keeper()` |
| `ORACLE_ADDRESS` | from deploy JSON | oracle address override |
| `DEPLOYMENTS_PATH` | `../contracts/deployments/sepolia.json` | source for `oracle` address |
| `VOLX_API_URL` | `http://localhost:8090` | VolX REST base URL |
| `POLL_INTERVAL_MS` | `60000` | poll cadence |
| `DEVIATION_BPS` | `50` | push if either index moves ≥ this (0.5%) |
| `HEARTBEAT_MS` | `1800000` | force a push after this age (30m) |

## Push policy

Each cycle fetches both indices, reads the current on-chain values, and decides:

1. **init** — either feed never set on-chain → push.
2. **deviation** — `|new − onchain| / onchain` ≥ `DEVIATION_BPS` for either feed → push.
3. **heartbeat** — staler feed older than `HEARTBEAT_MS` → push.
4. otherwise **skip** (no tx, logged).

Values are scaled to the oracle's fixed point: value × `1e8`, confidence × `1e6`
(clamped to ≤ 1.0). API failures back off exponentially and skip the cycle
rather than crash the loop.

## Test

```bash
pnpm test        # unit tests for the decision + scaling logic (node:test)
pnpm typecheck
```

End-to-end against a local anvil + the deployed oracle is exercised in the
keeper PR; see the decision logic in `src/decide.ts` and the loop in
`src/index.ts`.

> Testnet demo only (Sepolia). Not audited.
