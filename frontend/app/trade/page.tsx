"use client";

import { useCallback, useEffect, useState } from "react";
import type { Hex } from "viem";
import { Header } from "@/components/Header";
import { Footer } from "@/components/Footer";
import { WalletButton, NetworkGuard, Card, Stat, TxButton, cleanErr } from "@/components/dapp";
import { TradeChart } from "@/components/TradeChart";
import { useWallet } from "@/lib/wallet";
import { ADDRESSES, BPS, INDEX, type IndexKey, MAX_LEVERAGE, mockUsdcAbi, OPEN_FEE_BPS, perpAbi } from "@/lib/contracts";
import { fmtPnl, fmtPrice, fmtUsdc, parseUsdc } from "@/lib/format";
import {
  readOracle,
  readPositions,
  readUser,
  type OraclePrice,
  type UserBalances,
  type UserPosition,
} from "@/lib/perp";

const STALE_SECS = 3600;

export default function TradePage() {
  // Lifted here so the chart + market toggle live ABOVE the wallet gate
  // (index price is public — no wallet needed to view it).
  const [index, setIndex] = useState<IndexKey>("bvol");
  return (
    <div className="flex min-h-dvh flex-col">
      <Header />
      <main className="mx-auto w-full max-w-6xl flex-1 px-5 py-8">
        <div className="mb-5 flex items-end justify-between gap-4">
          <div>
            <h1 className="text-2xl font-semibold tracking-tight">Trade volatility</h1>
            <p className="mt-1 text-sm text-muted">
              Leveraged long/short on BVOL &amp; EVOL, settled against the LP vault. Testnet demo.
            </p>
          </div>
          <WalletButton />
        </div>

        {/* Market picker + chart — always visible, drive the order ticket below. */}
        <div className="mb-4 w-56">
          <Segmented
            value={index}
            onChange={(v) => setIndex(v as IndexKey)}
            options={[{ v: "bvol", l: "BVOL" }, { v: "evol", l: "EVOL" }]}
          />
        </div>
        <div className="mb-6">
          <TradeChart id={index} />
        </div>

        <NetworkGuard>
          <TradeInner index={index} />
        </NetworkGuard>
      </main>
      <Footer />
    </div>
  );
}

function TradeInner({ index }: { index: IndexKey }) {
  const { account, publicClient, walletClient } = useWallet();
  const [prices, setPrices] = useState<Record<IndexKey, OraclePrice | null>>({ bvol: null, evol: null });
  const [bal, setBal] = useState<UserBalances | null>(null);
  const [positions, setPositions] = useState<UserPosition[]>([]);
  const [nowSec, setNowSec] = useState(() => Math.floor(Date.now() / 1000));

  // form (index is chosen by the market picker above)
  const [isLong, setIsLong] = useState(true);
  const [leverage, setLeverage] = useState(2);
  const [collateral, setCollateral] = useState("100");

  const refresh = useCallback(async () => {
    if (!account) return;
    const [bvol, evol, user, pos] = await Promise.all([
      readOracle(publicClient, "bvol"),
      readOracle(publicClient, "evol"),
      readUser(publicClient, account),
      readPositions(publicClient, account),
    ]);
    setPrices({ bvol, evol });
    setBal(user);
    setPositions(pos);
    setNowSec(Math.floor(Date.now() / 1000));
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

  const price = prices[index];
  const priceUnset = !price || price.updatedAt === 0n;
  const priceStale = !!price && price.updatedAt > 0n && nowSec - Number(price.updatedAt) > STALE_SECS;
  const canOpen = !priceUnset && !priceStale;

  let collateralUnits = 0n;
  try {
    collateralUnits = collateral.trim() ? parseUsdc(collateral.trim()) : 0n;
  } catch {
    collateralUnits = 0n;
  }
  const needsApproval = !!bal && collateralUnits > 0n && bal.allowance < collateralUnits;
  // Mirror the on-chain open fee (collateral × lev × 0.1%): the contract reserves
  // notional on the post-fee working collateral, so the displayed value matches.
  const openFee = (collateralUnits * BigInt(leverage) * OPEN_FEE_BPS) / BPS;
  const working = collateralUnits > openFee ? collateralUnits - openFee : 0n;
  const notional = working * BigInt(leverage);

  const faucet = () =>
    send(() => walletClient.writeContract({ address: ADDRESSES.mockUSDC, abi: mockUsdcAbi, functionName: "faucet", chain: walletClient.chain, account }));

  const approve = () =>
    send(() => walletClient.writeContract({ address: ADDRESSES.mockUSDC, abi: mockUsdcAbi, functionName: "approve", args: [ADDRESSES.perp, collateralUnits], chain: walletClient.chain, account }));

  const open = () =>
    send(() => walletClient.writeContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "openPosition", args: [INDEX[index], isLong, collateralUnits, BigInt(leverage)], chain: walletClient.chain, account }));

  const close = (id: bigint) =>
    send(() => walletClient.writeContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "closePosition", args: [id], chain: walletClient.chain, account }));

  return (
    <div className="grid gap-6 lg:grid-cols-[380px_1fr]">
      {/* Order form */}
      <div className="flex flex-col gap-4">
        <Card title={`Open ${index.toUpperCase()} position`}>
          {/* Side */}
          <Segmented
            value={isLong ? "long" : "short"}
            onChange={(v) => setIsLong(v === "long")}
            options={[{ v: "long", l: "Long ▲", color: "up" }, { v: "short", l: "Short ▼", color: "down" }]}
          />

          {/* Collateral */}
          <div className="mt-4">
            <label className="text-[10px] font-medium uppercase tracking-[0.16em] text-soft">Collateral (mUSDC)</label>
            <input
              value={collateral}
              onChange={(e) => setCollateral(e.target.value)}
              inputMode="decimal"
              className="mt-1.5 w-full rounded-lg border border-border bg-background px-3 py-2.5 font-mono text-sm tabular-nums outline-none focus:border-accent"
            />
            <div className="mt-1 text-[11px] text-soft">
              Wallet: {bal ? fmtUsdc(bal.usdc) : "—"} mUSDC
            </div>
          </div>

          {/* Leverage */}
          <div className="mt-4">
            <div className="flex items-center justify-between">
              <label className="text-[10px] font-medium uppercase tracking-[0.16em] text-soft">Leverage</label>
              <span className="font-mono text-sm font-semibold text-accent">{leverage}×</span>
            </div>
            <input type="range" min={1} max={MAX_LEVERAGE} value={leverage} onChange={(e) => setLeverage(Number(e.target.value))} className="mt-2 w-full accent-[var(--accent)]" />
          </div>

          {/* Summary */}
          <div className="mt-4 grid grid-cols-2 gap-3 rounded-lg border border-border-subtle bg-background/40 p-3">
            <Stat label="Notional" value={`${fmtUsdc(notional)}`} />
            <Stat label={`${index.toUpperCase()} price`} value={price ? fmtPrice(price.value) : "—"} accent="accent" />
          </div>

          {/* Price health */}
          {priceUnset && <p className="mt-3 text-xs text-down">Oracle price unset — keeper offline. Opening is disabled until a price is pushed.</p>}
          {priceStale && <p className="mt-3 text-xs text-down">Oracle price is stale (&gt; 1h). Wait for the keeper to refresh.</p>}

          {/* Actions */}
          <div className="mt-4 flex flex-col gap-2">
            {needsApproval ? (
              <TxButton label={`Approve ${collateral} mUSDC`} onRun={approve} disabled={collateralUnits === 0n} />
            ) : (
              <TxButton
                label={`Open ${isLong ? "long" : "short"} ${leverage}×`}
                variant={isLong ? "up" : "down"}
                onRun={open}
                disabled={!canOpen || collateralUnits === 0n || (!!bal && collateralUnits > bal.usdc)}
              />
            )}
          </div>
        </Card>

        <Card title="Faucet">
          <p className="text-xs text-muted">Need test collateral? Mint 10,000 mUSDC to your wallet.</p>
          <div className="mt-3">
            <TxButton label="Claim 10,000 mUSDC" onRun={faucet} />
          </div>
        </Card>
      </div>

      {/* Positions */}
      <div className="flex flex-col gap-4">
        <Card title="Your positions">
          {positions.length === 0 ? (
            <p className="py-6 text-center text-sm text-soft">No open positions.</p>
          ) : (
            <div className="flex flex-col divide-y divide-border-subtle">
              {positions.map((p) => (
                <PositionRow key={p.id.toString()} p={p} onClose={() => close(p.id)} />
              ))}
            </div>
          )}
        </Card>
      </div>
    </div>
  );
}

function PositionRow({ p, onClose }: { p: UserPosition; onClose: () => Promise<void> }) {
  const win = p.pnl >= 0n;
  return (
    <div className="flex items-center justify-between gap-3 py-3">
      <div className="flex items-center gap-3">
        <span className={`rounded-md px-2 py-0.5 text-[10px] font-bold uppercase ${p.isLong ? "bg-up-soft text-up" : "bg-down-soft text-down"}`}>
          {p.isLong ? "Long" : "Short"} {Number(p.leverage)}×
        </span>
        <div>
          <div className="text-sm font-semibold">{p.index.toUpperCase()}</div>
          <div className="font-mono text-[11px] text-soft">
            entry {fmtPrice(p.entryPrice)} · coll {fmtUsdc(p.collateral)}
          </div>
        </div>
      </div>
      <div className="flex items-center gap-4">
        <div className="text-right">
          <div className={`font-mono text-sm font-semibold tabular-nums ${win ? "text-up" : "text-down"}`}>{fmtPnl(p.pnl)}</div>
          <div className="font-mono text-[11px] text-soft">eq {fmtUsdc(p.equity)}</div>
        </div>
        {p.liquidatable && <span className="rounded bg-down-soft px-1.5 py-0.5 text-[10px] font-bold text-down">LIQ</span>}
        <CloseButton onClose={onClose} />
      </div>
    </div>
  );
}

function CloseButton({ onClose }: { onClose: () => Promise<void> }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  return (
    <div className="flex flex-col items-end">
      <button
        onClick={async () => {
          setErr(null);
          setBusy(true);
          try {
            await onClose();
          } catch (e) {
            setErr(cleanErr(e));
          } finally {
            setBusy(false);
          }
        }}
        disabled={busy}
        className="rounded-lg border border-border px-3 py-1.5 text-xs font-semibold transition-colors hover:border-accent hover:text-accent disabled:opacity-50"
      >
        {busy ? "…" : "Close"}
      </button>
      {err && <span className="mt-1 max-w-[160px] text-right text-[10px] text-down">{err}</span>}
    </div>
  );
}

function Segmented({
  value,
  onChange,
  options,
}: {
  value: string;
  onChange: (v: string) => void;
  options: { v: string; l: string; color?: "up" | "down" }[];
}) {
  return (
    <div className="grid grid-flow-col gap-1 rounded-lg border border-border-subtle bg-background/40 p-1">
      {options.map((o) => {
        const active = o.v === value;
        const activeColor = o.color === "up" ? "bg-up-soft text-up" : o.color === "down" ? "bg-down-soft text-down" : "bg-accent-soft text-accent";
        return (
          <button
            key={o.v}
            onClick={() => onChange(o.v)}
            className={`rounded-md px-3 py-2 text-sm font-semibold transition-colors ${active ? activeColor : "text-muted hover:text-foreground"}`}
          >
            {o.l}
          </button>
        );
      })}
    </div>
  );
}
