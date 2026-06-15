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
import { oracleAbi, INDEX } from "./oracleAbi.js";
import type { OnChain } from "./decide.js";
import type { IndexQuote } from "./api.js";

export class OracleClient {
  private pub: PublicClient;
  private wallet: WalletClient;
  private account;
  private oracle: Address;
  private chainReady: ReturnType<typeof defineChain> | null = null;
  // Stuck-tx recovery state: when consecutive pushes resolve to the SAME
  // nonce (the prior tx is still pending), we resend at that nonce with a
  // strictly higher tip so the replacement is accepted instead of rejected
  // as "replacement transaction underpriced".
  private lastNonce: number | null = null;
  private tip = 2_000_000_000n; // 2 gwei priority fee, escalates on retry

  constructor(rpcUrl: string, privateKey: Hex, oracle: Address) {
    this.account = privateKeyToAccount(privateKey);
    this.oracle = oracle;
    this.pub = createPublicClient({ transport: http(rpcUrl) });
    this.wallet = createWalletClient({ account: this.account, transport: http(rpcUrl) });
  }

  get signer(): Address {
    return this.account.address;
  }

  /** Lazily resolve the chain so writeContract has a chain object regardless of
   * network (anvil 31337 / sepolia 11155111). */
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

  /** The oracle's configured keeper address (for a startup sanity check). */
  async keeper(): Promise<Address> {
    return this.pub.readContract({ address: this.oracle, abi: oracleAbi, functionName: "keeper" });
  }

  async readPrice(index: 0 | 1): Promise<OnChain> {
    const [value, updatedAt] = await this.pub.readContract({
      address: this.oracle,
      abi: oracleAbi,
      functionName: "getPrice",
      args: [index],
    });
    return { value, updatedAt };
  }

  async readBoth(): Promise<{ bvol: OnChain; evol: OnChain }> {
    const [bvol, evol] = await Promise.all([this.readPrice(INDEX.BVOL), this.readPrice(INDEX.EVOL)]);
    return { bvol, evol };
  }

  /** Compute EIP-1559 fees with generous headroom for volatile testnet gas,
   * pinned to the first unconfirmed nonce so a stuck tx is *replaced* rather
   * than queued behind itself. If the prior push is still pending (same nonce
   * as last time), escalate the tip by +30% so the replacement clears the
   * "underpriced" check. */
  private async fees(): Promise<{ maxFeePerGas: bigint; maxPriorityFeePerGas: bigint; nonce: number }> {
    const block = await this.pub.getBlock({ blockTag: "latest" });
    const base = block.baseFeePerGas ?? 1_000_000_000n;
    const nonce = await this.pub.getTransactionCount({ address: this.account.address, blockTag: "latest" });

    if (nonce === this.lastNonce) {
      // Prior tx still stuck → bump tip 30%, capped at 50 gwei so a tx wedged
      // across many ticks can't escalate the fee unboundedly into RPC-reject
      // territory (which would permanently jam the keeper).
      this.tip = (this.tip * 13n) / 10n;
      if (this.tip > 50_000_000_000n) this.tip = 50_000_000_000n;
    } else {
      this.tip = 2_000_000_000n; // nonce advanced (mined) → reset to baseline
      this.lastNonce = nonce;
    }

    // 3x base covers a basefee spike between estimate and inclusion; +tip on top.
    return { maxFeePerGas: base * 3n + this.tip, maxPriorityFeePerGas: this.tip, nonce };
  }

  /** Push both indices in one tx and wait for the receipt. Returns the hash. */
  async pushBoth(bvol: IndexQuote, evol: IndexQuote): Promise<Hex> {
    const chain = await this.chain();
    const { maxFeePerGas, maxPriorityFeePerGas, nonce } = await this.fees();
    const hash = await this.wallet.writeContract({
      account: this.account,
      chain,
      address: this.oracle,
      abi: oracleAbi,
      functionName: "updateBoth",
      // confScaled is a JS number but clamped to [0, 1e6] in api.ts, well within
      // uint32 — viem accepts number for small uint widths.
      args: [bvol.valueScaled, bvol.confScaled, evol.valueScaled, evol.confScaled],
      // Explicit fees + nonce: testnet basefee is volatile and viem's default
      // estimate runs too thin, leaving txs stuck. See fees().
      maxFeePerGas,
      maxPriorityFeePerGas,
      nonce,
    });
    // Bounded wait: a dropped/stuck tx would otherwise hang the loop forever.
    // The timeout throws, propagates to the tick catch, and the loop continues;
    // the next tick replaces this nonce with a higher tip via fees().
    await this.pub.waitForTransactionReceipt({ hash, timeout: 120_000 });
    // Confirmed: clear the pending-nonce latch so fees() resets the tip.
    this.lastNonce = null;
    return hash;
  }
}
