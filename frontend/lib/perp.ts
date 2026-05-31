import type { Address, PublicClient } from "viem";
import { ADDRESSES, INDEX, type IndexKey, mockUsdcAbi, oracleAbi, perpAbi } from "./contracts";

/// Liquidation loss threshold as a fraction of collateral (VolXPerp
/// LIQ_THRESHOLD_BPS = 8000 = 80%).
export const LIQ_THRESHOLD = 0.8;

/// Price (in index vol points) at which a position becomes liquidatable.
/// Derived from the contract rule: liquidatable once unrealized loss >=
/// LIQ_THRESHOLD * collateral. Loss = notional*|move|/entry and
/// notional = collateral*leverage, so collateral cancels and the trigger is
/// a pure function of entry, leverage and side:
///   long:  mark = entry * (1 - 0.8/leverage)
///   short: mark = entry * (1 + 0.8/leverage)
export function liqPriceVol(entryVol: number, leverage: number, isLong: boolean): number {
  if (leverage <= 0) return entryVol;
  const move = LIQ_THRESHOLD / leverage;
  return isLong ? entryVol * (1 - move) : entryVol * (1 + move);
}

/// Convert an oracle/entry value (1e8 fixed-point bigint) to a vol-points number.
export function toVol(value1e8: bigint): number {
  return Number(value1e8) / 1e8;
}

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
  pnl: bigint; // signed — may be negative (int256); net of funding via equity
  equity: bigint; // collateral + pnl - funding, floored 0
  funding: bigint; // accrued borrow fee so far
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
      { ...perp, functionName: "accruedFunding" as const, args: [id] },
    ]),
  });

  const out: UserPosition[] = [];
  for (let k = 0; k < mine.length; k++) {
    const item = mine[k];
    if (!item) continue;
    const pv = values[k * 3];
    if (!pv || pv.status === "failure") continue; // position no longer exists
    const liq = values[k * 3 + 1];
    const fnd = values[k * 3 + 2];
    const [pnl, equity] = pv.result as readonly [bigint /* int256, signed */, bigint];
    const liquidatable = liq && liq.status === "success" ? (liq.result as boolean) : false;
    const funding = fnd && fnd.status === "success" ? (fnd.result as bigint) : 0n;
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
      funding,
      liquidatable,
    });
  }
  return out;
}

export type OrderKind = "limit" | "tp" | "sl";

export interface OrderItem {
  id: bigint;
  kind: OrderKind;
  index: IndexKey;
  isLong: boolean;
  collateral: bigint; // escrow (limit only)
  leverage: bigint;
  triggerPrice: bigint; // 1e8
  triggerAbove: boolean;
  positionId: bigint;
}

const ORDER_KIND: OrderKind[] = ["limit", "tp", "sl"];

/** Scan [0, nextOrderId) for live orders owned by `account`. */
export async function readOrders(client: PublicClient, account: Address): Promise<OrderItem[]> {
  const next = await client.readContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "nextOrderId" });
  const n = Number(next);
  if (n === 0) return [];
  const perp = { address: ADDRESSES.perp, abi: perpAbi } as const;
  const rows = await client.multicall({
    allowFailure: false,
    contracts: Array.from({ length: n }, (_, i) => ({ ...perp, functionName: "orders" as const, args: [BigInt(i)] })),
  });
  const out: OrderItem[] = [];
  rows.forEach((o, i) => {
    const trader = o[0] as Address;
    if (trader.toLowerCase() !== account.toLowerCase()) return; // includes the zero-address (consumed/cancelled) rows
    out.push({
      id: BigInt(i),
      kind: ORDER_KIND[Number(o[1])] ?? "limit",
      index: Number(o[2]) === INDEX.bvol ? "bvol" : "evol",
      isLong: o[3] as boolean,
      collateral: o[4] as bigint,
      leverage: o[5] as bigint,
      triggerPrice: o[6] as bigint,
      triggerAbove: o[7] as boolean,
      positionId: o[8] as bigint,
    });
  });
  return out;
}

/** Current borrow-fee (funding) rate, in bps/day. */
export async function readFundingRate(client: PublicClient): Promise<bigint> {
  return client.readContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "fundingBpsPerDay" });
}
