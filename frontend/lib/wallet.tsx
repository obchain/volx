"use client";

import { createContext, useCallback, useContext, useEffect, useMemo, useState } from "react";
import {
  createPublicClient,
  createWalletClient,
  custom,
  http,
  type Address,
  type EIP1193Provider,
  type PublicClient,
  type WalletClient,
} from "viem";
import { sepolia } from "viem/chains";
import { READ_RPC_URL, SEPOLIA_CHAIN_ID } from "./contracts";

declare global {
  interface Window {
    ethereum?: EIP1193Provider;
  }
}

interface WalletState {
  account: Address | null;
  chainId: number | null;
  isSepolia: boolean;
  hasProvider: boolean;
  connecting: boolean;
  connect: () => Promise<void>;
  switchToSepolia: () => Promise<void>;
  /** read-only client over a public RPC; always available */
  publicClient: PublicClient;
  /** signing client over the injected provider; null until connected */
  walletClient: WalletClient | null;
}

const Ctx = createContext<WalletState | null>(null);

const SEPOLIA_HEX = "0xaa36a7"; // 11155111

export function WalletProvider({ children }: { children: React.ReactNode }) {
  const [account, setAccount] = useState<Address | null>(null);
  const [chainId, setChainId] = useState<number | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [hasProvider, setHasProvider] = useState(false);

  const publicClient = useMemo(
    () => createPublicClient({ chain: sepolia, transport: http(READ_RPC_URL) }) as PublicClient,
    [],
  );

  const walletClient = useMemo<WalletClient | null>(() => {
    if (!account || typeof window === "undefined" || !window.ethereum) return null;
    return createWalletClient({ account, chain: sepolia, transport: custom(window.ethereum) });
  }, [account]);

  // Pick up an already-authorized account + wire provider events.
  useEffect(() => {
    const eth = typeof window !== "undefined" ? window.ethereum : undefined;
    if (!eth) return;
    setHasProvider(true);

    eth.request({ method: "eth_accounts" }).then((accs) => {
      const list = accs as Address[];
      if (list.length > 0) setAccount(list[0] ?? null);
    });
    eth.request({ method: "eth_chainId" }).then((id) => setChainId(Number(id as string)));

    const onAccounts = (...args: unknown[]) => {
      const accs = args[0] as Address[];
      setAccount(accs.length > 0 ? (accs[0] ?? null) : null);
    };
    const onChain = (...args: unknown[]) => setChainId(Number(args[0] as string));
    // EIP-1193 providers expose on/removeListener; viem's type omits them.
    const p = eth as unknown as {
      on: (e: string, h: (...a: unknown[]) => void) => void;
      removeListener: (e: string, h: (...a: unknown[]) => void) => void;
    };
    p.on("accountsChanged", onAccounts);
    p.on("chainChanged", onChain);
    return () => {
      p.removeListener("accountsChanged", onAccounts);
      p.removeListener("chainChanged", onChain);
    };
  }, []);

  const connect = useCallback(async () => {
    if (typeof window === "undefined") return;
    const eth = window.ethereum;
    if (!eth) {
      window.open("https://metamask.io/download/", "_blank");
      return;
    }
    setConnecting(true);
    try {
      const accs = (await eth.request({ method: "eth_requestAccounts" })) as Address[];
      setAccount(accs[0] ?? null);
      const id = (await eth.request({ method: "eth_chainId" })) as string;
      setChainId(Number(id));
    } finally {
      setConnecting(false);
    }
  }, []);

  const switchToSepolia = useCallback(async () => {
    if (typeof window === "undefined") return;
    const eth = window.ethereum;
    if (!eth) return;
    try {
      await eth.request({ method: "wallet_switchEthereumChain", params: [{ chainId: SEPOLIA_HEX }] });
    } catch (e) {
      // 4902 = chain not added to the wallet yet; add it then retry implicitly.
      if ((e as { code?: number }).code === 4902) {
        await eth.request({
          method: "wallet_addEthereumChain",
          params: [
            {
              chainId: SEPOLIA_HEX,
              chainName: "Sepolia",
              nativeCurrency: { name: "Sepolia ETH", symbol: "ETH", decimals: 18 },
              rpcUrls: [READ_RPC_URL],
              blockExplorerUrls: ["https://sepolia.etherscan.io"],
            },
          ],
        });
      } else {
        throw e;
      }
    }
  }, []);

  const value: WalletState = {
    account,
    chainId,
    isSepolia: chainId === SEPOLIA_CHAIN_ID,
    hasProvider,
    connecting,
    connect,
    switchToSepolia,
    publicClient,
    walletClient,
  };

  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useWallet(): WalletState {
  const v = useContext(Ctx);
  if (!v) throw new Error("useWallet must be used within WalletProvider");
  return v;
}
