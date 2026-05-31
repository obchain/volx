import type { Address, PublicClient } from "viem";
import { ADDRESSES, perpAbi } from "./contracts";
import { readVault, type VaultStats } from "./perp";

export interface ProtocolStats {
  vault: VaultStats;
  /** assets per vxLP share (1.0 at genesis). */
  sharePrice: number;
  /** reserved / totalAssets, %. */
  utilizationPct: number;
  openPositions: number;
  longs: number;
  shorts: number;
  /** summed open notional (collateral*leverage), base units. */
  openNotional: bigint;
}

/** Read vault + aggregate open-interest stats. Public — no wallet needed.
 * Scans [0, nextPositionId) which is fine for the demo's small id space. */
export async function readProtocolStats(client: PublicClient): Promise<ProtocolStats> {
  const vault = await readVault(client);
  const next = await client.readContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "nextPositionId" });
  const n = Number(next);

  let openPositions = 0;
  let longs = 0;
  let shorts = 0;
  let openNotional = 0n;

  if (n > 0) {
    const perp = { address: ADDRESSES.perp, abi: perpAbi } as const;
    const rows = await client.multicall({
      allowFailure: false,
      contracts: Array.from({ length: n }, (_, i) => ({ ...perp, functionName: "positions" as const, args: [BigInt(i)] })),
    });
    for (const p of rows) {
      const trader = p[0] as Address;
      if (trader === "0x0000000000000000000000000000000000000000") continue;
      openPositions++;
      if (p[2] as boolean) longs++;
      else shorts++;
      openNotional += (p[3] as bigint) * (p[4] as bigint); // collateral * leverage
    }
  }

  const sharePrice = vault.totalSupply > 0n ? Number(vault.totalAssets) / Number(vault.totalSupply) : 1;
  const utilizationPct = vault.totalAssets > 0n ? Number((vault.reserved * 10_000n) / vault.totalAssets) / 100 : 0;

  return { vault, sharePrice, utilizationPct, openPositions, longs, shorts, openNotional };
}
