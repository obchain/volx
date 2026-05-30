import type { Address, PublicClient } from "viem";
import { ADDRESSES, INDEX, type IndexKey, mockUsdcAbi, oracleAbi, perpAbi } from "./contracts";

export interface OraclePrice {
  value: bigint; // 1e8
  updatedAt: bigint; // unix seconds; 0 = never set
}

export async function readOracle(client: PublicClient, index: IndexKey): Promise<OraclePrice> {
  const [value, updatedAt] = await client.readContract({
    address: ADDRESSES.oracle,
    abi: oracleAbi,
    functionName: "getPrice",
    args: [INDEX[index]],
  });
  return { value, updatedAt };
}

export interface VaultStats {
  totalAssets: bigint;
  reserved: bigint;
  available: bigint;
  totalSupply: bigint;
}

export async function readVault(client: PublicClient): Promise<VaultStats> {
  const perp = { address: ADDRESSES.perp, abi: perpAbi } as const;
  const [totalAssets, reserved, available, totalSupply] = await client.multicall({
    allowFailure: false,
    contracts: [
      { ...perp, functionName: "totalAssets" },
      { ...perp, functionName: "reservedAssets" },
      { ...perp, functionName: "availableAssets" },
      { ...perp, functionName: "totalSupply" },
    ],
  });
  return { totalAssets, reserved, available, totalSupply };
}

export interface UserBalances {
  usdc: bigint;
  shares: bigint;
  shareAssets: bigint; // convertToAssets(shares)
  allowance: bigint;
}

export async function readUser(client: PublicClient, account: Address): Promise<UserBalances> {
  const [usdc, allowance, shares] = await client.multicall({
    allowFailure: false,
    contracts: [
      { address: ADDRESSES.mockUSDC, abi: mockUsdcAbi, functionName: "balanceOf", args: [account] },
      { address: ADDRESSES.mockUSDC, abi: mockUsdcAbi, functionName: "allowance", args: [account, ADDRESSES.perp] },
      { address: ADDRESSES.perp, abi: perpAbi, functionName: "balanceOf", args: [account] },
    ],
  });
  const shareAssets =
    shares === 0n
      ? 0n
      : await client.readContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "convertToAssets", args: [shares] });
  return { usdc, shares, shareAssets, allowance };
}

export interface UserPosition {
  id: bigint;
  index: IndexKey;
  isLong: boolean;
  collateral: bigint;
  leverage: bigint;
  entryPrice: bigint;
  pnl: bigint; // signed — may be negative (int256)
  equity: bigint;
  liquidatable: boolean;
}

/** Scan [0, nextPositionId) for positions owned by `account`. Fine for the
 * demo's small id space; production would index PositionOpened events. */
export async function readPositions(client: PublicClient, account: Address): Promise<UserPosition[]> {
  const next = await client.readContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "nextPositionId" });
  const n = Number(next);
  if (n === 0) return [];

  const perp = { address: ADDRESSES.perp, abi: perpAbi } as const;
  const raw = await client.multicall({
    allowFailure: false,
    contracts: Array.from({ length: n }, (_, i) => ({ ...perp, functionName: "positions" as const, args: [BigInt(i)] })),
  });

  const mine = raw
    .map((p, i) => ({ p, id: BigInt(i) }))
    .filter(({ p }) => (p[0] as Address).toLowerCase() === account.toLowerCase());
  if (mine.length === 0) return [];

  // allowFailure so a position closed/liquidated between the two multicalls
  // (positionValue then reverts PositionNotFound) drops out instead of throwing
  // and freezing the whole list.
  const values = await client.multicall({
    allowFailure: true,
    contracts: mine.flatMap(({ id }) => [
      { ...perp, functionName: "positionValue" as const, args: [id] },
      { ...perp, functionName: "isLiquidatable" as const, args: [id] },
    ]),
  });

  const out: UserPosition[] = [];
  for (let k = 0; k < mine.length; k++) {
    const item = mine[k];
    if (!item) continue;
    const pv = values[k * 2];
    if (!pv || pv.status === "failure") continue; // position no longer exists
    const liq = values[k * 2 + 1];
    const [pnl, equity] = pv.result as readonly [bigint /* int256, signed */, bigint];
    const liquidatable = liq && liq.status === "success" ? (liq.result as boolean) : false;
    const p = item.p;
    out.push({
      id: item.id,
      index: Number(p[1]) === INDEX.bvol ? "bvol" : "evol",
      isLong: p[2] as boolean,
      collateral: p[3] as bigint,
      leverage: p[4] as bigint,
      entryPrice: p[5] as bigint,
      pnl,
      equity,
      liquidatable,
    });
  }
  return out;
}
