"use client";

import { useCallback, useEffect, useState } from "react";
import type { Hex } from "viem";
import { Header } from "@/components/Header";
import { Footer } from "@/components/Footer";
import { WalletButton, NetworkGuard, Card, Stat, TxButton } from "@/components/dapp";
import { useWallet } from "@/lib/wallet";
import { ADDRESSES, mockUsdcAbi, perpAbi } from "@/lib/contracts";
import { fmtUsdc, parseUsdc } from "@/lib/format";
import { readUser, readVault, type UserBalances, type VaultStats } from "@/lib/perp";

export default function PoolPage() {
  return (
    <div className="flex min-h-dvh flex-col">
      <Header />
      <main className="mx-auto w-full max-w-5xl flex-1 px-5 py-8">
        <div className="mb-6 flex items-center justify-between">
          <div>
            <h1 className="text-2xl font-semibold tracking-tight">Liquidity pool</h1>
            <p className="mt-1 text-sm text-muted">
              Provide mUSDC as the counterparty to all trades. Earn fees + trader losses; bear trader wins.
            </p>
          </div>
          <WalletButton />
        </div>
        <PoolStats />
        <div className="mt-6">
          <NetworkGuard>
            <PoolInner />
          </NetworkGuard>
        </div>
      </main>
      <Footer />
    </div>
  );
}

/** Vault-wide stats — visible without a wallet (read over the public RPC). */
function PoolStats() {
  const { publicClient } = useWallet();
  const [v, setV] = useState<VaultStats | null>(null);

  useEffect(() => {
    let alive = true;
    const load = () => readVault(publicClient).then((s) => alive && setV(s)).catch(() => {});
    load();
    const t = setInterval(load, 15_000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [publicClient]);

  const util = v && v.totalAssets > 0n ? Number((v.reserved * 10_000n) / v.totalAssets) / 100 : 0;
  const sharePrice = v && v.totalSupply > 0n ? Number(v.totalAssets) / Number(v.totalSupply) : 1;

  return (
    <Card>
      <div className="grid grid-cols-2 gap-5 sm:grid-cols-4">
        <Stat label="TVL" value={v ? `${fmtUsdc(v.totalAssets)}` : "—"} accent="accent" />
        <Stat label="Available" value={v ? fmtUsdc(v.available) : "—"} />
        <Stat label="Reserved (OI)" value={v ? fmtUsdc(v.reserved) : "—"} />
        <Stat label="Utilization" value={v ? `${util.toFixed(1)}%` : "—"} accent={util > 80 ? "down" : undefined} />
      </div>
      <div className="mt-4 border-t border-border-subtle pt-4">
        <Stat label="Share price (mUSDC / vxLP)" value={sharePrice.toFixed(6)} />
      </div>
    </Card>
  );
}

function PoolInner() {
  const { account, publicClient, walletClient } = useWallet();
  const [bal, setBal] = useState<UserBalances | null>(null);
  const [vault, setVault] = useState<VaultStats | null>(null);
  const [depositAmt, setDepositAmt] = useState("1000");
  const [withdrawShares, setWithdrawShares] = useState("");

  const refresh = useCallback(async () => {
    if (!account) return;
    const [u, v] = await Promise.all([readUser(publicClient, account), readVault(publicClient)]);
    setBal(u);
    setVault(v);
  }, [account, publicClient]);

  useEffect(() => {
    let alive = true;
    const run = () => alive && refresh();
    run();
    const t = setInterval(run, 12_000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [refresh]);

  const send = useCallback(
    async (fn: () => Promise<Hex>) => {
      const hash = await fn();
      await publicClient.waitForTransactionReceipt({ hash });
      await refresh();
    },
    [publicClient, refresh],
  );

  if (!walletClient || !account) return null;

  let depositUnits = 0n;
  try {
    depositUnits = depositAmt.trim() ? parseUsdc(depositAmt.trim()) : 0n;
  } catch {
    depositUnits = 0n;
  }
  let shareUnits = 0n;
  try {
    shareUnits = withdrawShares.trim() ? parseUsdc(withdrawShares.trim()) : 0n; // vxLP shares mirror 6dp
  } catch {
    shareUnits = 0n;
  }

  const needsApproval = !!bal && depositUnits > 0n && bal.allowance < depositUnits;

  const approve = () =>
    send(() => walletClient.writeContract({ address: ADDRESSES.mockUSDC, abi: mockUsdcAbi, functionName: "approve", args: [ADDRESSES.perp, depositUnits], chain: walletClient.chain, account }));
  const deposit = () =>
    send(() => walletClient.writeContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "deposit", args: [depositUnits], chain: walletClient.chain, account }));
  const withdraw = () =>
    send(() => walletClient.writeContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "withdraw", args: [shareUnits], chain: walletClient.chain, account }));

  return (
    <div className="grid gap-6 lg:grid-cols-2">
      {/* Deposit */}
      <Card title="Deposit">
        <label className="text-[10px] font-medium uppercase tracking-[0.16em] text-soft">Amount (mUSDC)</label>
        <input
          value={depositAmt}
          onChange={(e) => setDepositAmt(e.target.value)}
          inputMode="decimal"
          className="mt-1.5 w-full rounded-lg border border-border bg-background px-3 py-2.5 font-mono text-sm tabular-nums outline-none focus:border-accent"
        />
        <div className="mt-1 flex justify-between text-[11px] text-soft">
          <span>Wallet: {bal ? fmtUsdc(bal.usdc) : "—"} mUSDC</span>
          {bal && <button className="text-accent hover:underline" onClick={() => setDepositAmt(fmtUsdc(bal.usdc).replace(/,/g, ""))}>Max</button>}
        </div>
        <div className="mt-4">
          {needsApproval ? (
            <TxButton label={`Approve ${depositAmt} mUSDC`} onRun={approve} disabled={depositUnits === 0n} />
          ) : (
            <TxButton label="Deposit" onRun={deposit} disabled={depositUnits === 0n || (!!bal && depositUnits > bal.usdc)} />
          )}
        </div>
      </Card>

      {/* Withdraw */}
      <Card title="Withdraw">
        <label className="text-[10px] font-medium uppercase tracking-[0.16em] text-soft">Shares (vxLP)</label>
        <input
          value={withdrawShares}
          onChange={(e) => setWithdrawShares(e.target.value)}
          inputMode="decimal"
          placeholder="0.00"
          className="mt-1.5 w-full rounded-lg border border-border bg-background px-3 py-2.5 font-mono text-sm tabular-nums outline-none focus:border-accent"
        />
        <div className="mt-1 flex justify-between text-[11px] text-soft">
          <span>
            Holdings: {bal ? fmtUsdc(bal.shares) : "—"} vxLP ≈ {bal ? fmtUsdc(bal.shareAssets) : "—"} mUSDC
          </span>
          {bal && bal.shares > 0n && (
            <button className="text-accent hover:underline" onClick={() => setWithdrawShares(fmtUsdc(bal.shares).replace(/,/g, ""))}>Max</button>
          )}
        </div>
        {vault && bal && shareUnits > 0n && (
          <p className="mt-2 text-[11px] text-soft">
            {vault.available < (bal.shareAssets * shareUnits) / (bal.shares === 0n ? 1n : bal.shares)
              ? "Note: part of the pool is reserved against open interest and may block a full withdrawal."
              : ""}
          </p>
        )}
        <div className="mt-4">
          <TxButton label="Withdraw" variant="accent" onRun={withdraw} disabled={shareUnits === 0n || (!!bal && shareUnits > bal.shares)} />
        </div>
      </Card>

      {/* My position */}
      <Card title="Your liquidity" className="lg:col-span-2">
        <div className="grid grid-cols-2 gap-5 sm:grid-cols-3">
          <Stat label="vxLP shares" value={bal ? fmtUsdc(bal.shares) : "—"} />
          <Stat label="Redeemable" value={bal ? `${fmtUsdc(bal.shareAssets)} mUSDC` : "—"} accent="accent" />
          <Stat
            label="Pool share"
            value={bal && vault && vault.totalSupply > 0n ? `${((Number(bal.shares) / Number(vault.totalSupply)) * 100).toFixed(2)}%` : "—"}
          />
        </div>
      </Card>
    </div>
  );
}
