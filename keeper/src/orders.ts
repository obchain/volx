import {
  createPublicClient,
  createWalletClient,
  http,
  defineChain,
  type Address,
  type Hex,
  type PublicClient,
  type WalletClient,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { perpAbi } from "./perpAbi.js";
import { oracleAbi } from "./oracleAbi.js";
import { log } from "./log.js";

const ZERO = "0x0000000000000000000000000000000000000000";

/** Watches VolXPerpV2 conditional orders and executes the ones whose trigger
 * the oracle price has crossed. Trigger is checked off-chain first (against the
 * same oracle the contract reads) so we only spend gas on orders that will
 * actually execute. */
export class PerpExecutor {
  private pub: PublicClient;
  private wallet: WalletClient;
  private account;
  private perp: Address;
  private oracle: Address;
  private chainReady: ReturnType<typeof defineChain> | null = null;

  constructor(rpcUrl: string, privateKey: Hex, perp: Address, oracle: Address) {
    this.account = privateKeyToAccount(privateKey);
    this.perp = perp;
    this.oracle = oracle;
    this.pub = createPublicClient({ transport: http(rpcUrl) });
    this.wallet = createWalletClient({ account: this.account, transport: http(rpcUrl) });
  }

  private async chain() {
    if (this.chainReady) return this.chainReady;
    const id = await this.pub.getChainId();
    this.chainReady = defineChain({
      id,
      name: `chain-${id}`,
      nativeCurrency: { name: "Ether", symbol: "ETH", decimals: 18 },
      rpcUrls: { default: { http: [] } },
    });
    return this.chainReady;
  }

  /** One scan: execute every triggered order. Returns the count executed. */
  async tick(): Promise<number> {
    const next = await this.pub.readContract({ address: this.perp, abi: perpAbi, functionName: "nextOrderId" });
    const n = Number(next);
    if (n === 0) return 0;

    const perp = { address: this.perp, abi: perpAbi } as const;
    const rows = await this.pub.multicall({
      allowFailure: false,
      contracts: Array.from({ length: n }, (_, i) => ({ ...perp, functionName: "orders" as const, args: [BigInt(i)] })),
    });

    let executed = 0;
    for (let i = 0; i < n; i++) {
      const o = rows[i];
      if (!o) continue;
      const trader = o[0] as Address;
      if (trader === ZERO) continue; // consumed / cancelled

      const index = Number(o[2]) as 0 | 1;
      const triggerPrice = o[6] as bigint;
      const triggerAbove = o[7] as boolean;

      const [value] = await this.pub.readContract({ address: this.oracle, abi: oracleAbi, functionName: "getPrice", args: [index] });
      const mark = value as bigint;
      const triggered = triggerAbove ? mark >= triggerPrice : mark <= triggerPrice;
      if (!triggered) continue;

      try {
        const chain = await this.chain();
        const hash = await this.wallet.writeContract({
          account: this.account,
          chain,
          address: this.perp,
          abi: perpAbi,
          functionName: "executeOrder",
          args: [BigInt(i)],
        });
        await this.pub.waitForTransactionReceipt({ hash, timeout: 120_000 });
        executed++;
        log.info("executed order", { id: i, mark: mark.toString(), trigger: triggerPrice.toString(), tx: hash });
      } catch (e) {
        log.warn("executeOrder failed", { id: i, err: (e as Error).message });
      }
    }
    return executed;
  }
}
