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

  /** Push both indices in one tx and wait for the receipt. Returns the hash. */
  async pushBoth(bvol: IndexQuote, evol: IndexQuote): Promise<Hex> {
    const chain = await this.chain();
    const hash = await this.wallet.writeContract({
      account: this.account,
      chain,
      address: this.oracle,
      abi: oracleAbi,
      functionName: "updateBoth",
      // confScaled is a JS number but clamped to [0, 1e6] in api.ts, well within
      // uint32 — viem accepts number for small uint widths.
      args: [bvol.valueScaled, bvol.confScaled, evol.valueScaled, evol.confScaled],
    });
    // Bounded wait: a dropped/stuck tx would otherwise hang the loop forever.
    // The timeout throws, propagates to the tick catch, and the loop continues.
    await this.pub.waitForTransactionReceipt({ hash, timeout: 120_000 });
    return hash;
  }
}
