"use client";

import { useCallback, useEffect, useMemo, useState } from "react";
import type { Hex } from "viem";
import { Header } from "@/components/Header";
import { Footer } from "@/components/Footer";
import { WalletButton, NetworkGuard, Card, Stat, TxButton, cleanErr } from "@/components/dapp";
import { TradeChart, type ChartPosition, type ChartPreview } from "@/components/TradeChart";
import { useWallet } from "@/lib/wallet";
import { ADDRESSES, BPS, INDEX, type IndexKey, MAX_LEVERAGE, mockUsdcAbi, OPEN_FEE_BPS, perpAbi } from "@/lib/contracts";
import { fmtPnl, fmtPrice, fmtUsdc, parseUsdc } from "@/lib/format";
import {
  liqPriceVol,
  readFundingRate,
  readOracle,
  readOrders,
  readPositions,
  readUser,
  toVol,
  type OraclePrice,
  type OrderItem,
  type UserBalances,
  type UserPosition,
} from "@/lib/perp";

const STALE_SECS = 3600;
const VALUE_SCALE = 100_000_000; // 1e8, vol -> oracle units

export default function TradePage() {
  const { account, publicClient } = useWallet();

  const [index, setIndex] = useState<IndexKey>("bvol");
  const [isLong, setIsLong] = useState(true);
  const [leverage, setLeverage] = useState(2);
  const [collateral, setCollateral] = useState("100");
  const [orderType, setOrderType] = useState<"market" | "limit">("market");
  const [trigger, setTrigger] = useState("");

  const [prices, setPrices] = useState<Record<IndexKey, OraclePrice | null>>({ bvol: null, evol: null });
  const [bal, setBal] = useState<UserBalances | null>(null);
  const [positions, setPositions] = useState<UserPosition[]>([]);
  const [orders, setOrders] = useState<OrderItem[]>([]);
  const [fundingRate, setFundingRate] = useState<bigint>(0n);
  const [nowSec, setNowSec] = useState(() => Math.floor(Date.now() / 1000));

  const refresh = useCallback(async () => {
    if (!account) return;
    const [bvol, evol, user, pos, ord, fr] = await Promise.all([
      readOracle(publicClient, "bvol"),
      readOracle(publicClient, "evol"),
      readUser(publicClient, account),
      readPositions(publicClient, account),
      readOrders(publicClient, account),
      readFundingRate(publicClient),
    ]);
    setPrices({ bvol, evol });
    setBal(user);
    setPositions(pos);
    setOrders(ord);
    setFundingRate(fr);
    setNowSec(Math.floor(Date.now() / 1000));
  }, [account, publicClient]);

  useEffect(() => {
    if (!account) {
      setPositions([]);
      setOrders([]);
      setBal(null);
      return;
    }
    let alive = true;
    const run = () => alive && refresh();
    run();
    const t = setInterval(run, 12_000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [refresh, account]);

  const chartPositions = useMemo<ChartPosition[]>(
    () =>
      positions
        .filter((p) => p.index === index)
        .map((p) => ({ id: p.id.toString(), isLong: p.isLong, leverage: Number(p.leverage), entry: toVol(p.entryPrice) })),
    [positions, index],
  );
  const preview = useMemo<ChartPreview | null>(() => (account ? { isLong, leverage } : null), [account, isLong, leverage]);

  return (
    <div className="flex min-h-dvh flex-col">
      <Header />
      <main className="mx-auto w-full max-w-6xl flex-1 px-5 py-8">
        <div className="mb-5 flex items-end justify-between gap-4">
          <div>
            <h1 className="text-2xl font-semibold tracking-tight">Trade volatility</h1>
            <p className="mt-1 text-sm text-muted">
              Leveraged long/short on BVOL &amp; EVOL with funding, limit &amp; stop orders. Settled against the LP vault. Testnet demo.
            </p>
          </div>
          <WalletButton />
        </div>

        <div className="mb-4 w-56">
          <Segmented value={index} onChange={(v) => setIndex(v as IndexKey)} options={[{ v: "bvol", l: "BVOL" }, { v: "evol", l: "EVOL" }]} />
        </div>
        <div className="mb-6">
          <TradeChart id={index} positions={chartPositions} preview={preview} />
        </div>

        <NetworkGuard>
          <OrderTicket
            index={index}
            isLong={isLong}
            setIsLong={setIsLong}
            leverage={leverage}
            setLeverage={setLeverage}
            collateral={collateral}
            setCollateral={setCollateral}
            orderType={orderType}
            setOrderType={setOrderType}
            trigger={trigger}
            setTrigger={setTrigger}
            prices={prices}
            bal={bal}
            positions={positions}
            orders={orders}
            fundingRate={fundingRate}
            nowSec={nowSec}
            refresh={refresh}
          />
        </NetworkGuard>
      </main>
      <Footer />
    </div>
  );
}

interface TicketProps {
  index: IndexKey;
  isLong: boolean;
  setIsLong: (b: boolean) => void;
  leverage: number;
  setLeverage: (n: number) => void;
  collateral: string;
  setCollateral: (s: string) => void;
  orderType: "market" | "limit";
  setOrderType: (s: "market" | "limit") => void;
  trigger: string;
  setTrigger: (s: string) => void;
  prices: Record<IndexKey, OraclePrice | null>;
  bal: UserBalances | null;
  positions: UserPosition[];
  orders: OrderItem[];
  fundingRate: bigint;
  nowSec: number;
  refresh: () => Promise<void>;
}

function OrderTicket(props: TicketProps) {
  const {
    index, isLong, setIsLong, leverage, setLeverage, collateral, setCollateral,
    orderType, setOrderType, trigger, setTrigger, prices, bal, positions, orders, fundingRate, nowSec, refresh,
  } = props;
  const { account, publicClient, walletClient } = useWallet();

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
  const markVol = price && price.value > 0n ? toVol(price.value) : null;

  let collateralUnits = 0n;
  try {
    collateralUnits = collateral.trim() ? parseUsdc(collateral.trim()) : 0n;
  } catch {
    collateralUnits = 0n;
  }
  const needsApproval = !!bal && collateralUnits > 0n && bal.allowance < collateralUnits;
  const openFee = (collateralUnits * BigInt(leverage) * OPEN_FEE_BPS) / BPS;
  const working = collateralUnits > openFee ? collateralUnits - openFee : 0n;
  const notional = working * BigInt(leverage);

  // Limit trigger (vol points -> 1e8). triggerAbove inferred from where the
  // trigger sits relative to the current mark.
  const triggerVol = Number(trigger);
  const triggerValid = orderType === "limit" ? Number.isFinite(triggerVol) && triggerVol > 0 : true;
  const triggerUnits = triggerValid && orderType === "limit" ? BigInt(Math.round(triggerVol * VALUE_SCALE)) : 0n;
  const triggerAbove = markVol !== null ? triggerVol >= markVol : true;

  const marketCanOpen = !priceUnset && !priceStale;
  const previewLiq = markVol !== null ? liqPriceVol(markVol, leverage, isLong) : null;
  const fundingPctDay = Number(fundingRate) / 100; // bps -> %

  const faucet = () => send(() => walletClient.writeContract({ address: ADDRESSES.mockUSDC, abi: mockUsdcAbi, functionName: "faucet", chain: walletClient.chain, account }));
  const approve = () => send(() => walletClient.writeContract({ address: ADDRESSES.mockUSDC, abi: mockUsdcAbi, functionName: "approve", args: [ADDRESSES.perp, collateralUnits], chain: walletClient.chain, account }));
  const open = () => send(() => walletClient.writeContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "openPosition", args: [INDEX[index], isLong, collateralUnits, BigInt(leverage)], chain: walletClient.chain, account }));
  const placeLimit = () => send(() => walletClient.writeContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "placeLimitOpen", args: [INDEX[index], isLong, collateralUnits, BigInt(leverage), triggerUnits, triggerAbove], chain: walletClient.chain, account }));
  const close = (id: bigint) => send(() => walletClient.writeContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "closePosition", args: [id], chain: walletClient.chain, account }));
  const cancel = (id: bigint) => send(() => walletClient.writeContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "cancelOrder", args: [id], chain: walletClient.chain, account }));
  const placeStop = (positionId: bigint, triggerPrice: bigint, above: boolean, takeProfit: boolean) =>
    send(() => walletClient.writeContract({ address: ADDRESSES.perp, abi: perpAbi, functionName: "placeStop", args: [positionId, triggerPrice, above, takeProfit], chain: walletClient.chain, account }));

  return (
    <div className="grid gap-6 lg:grid-cols-[380px_1fr]">
      {/* Order ticket */}
      <div className="flex flex-col gap-4">
        <Card title={`${index.toUpperCase()} order`}>
          <Segmented value={orderType} onChange={(v) => setOrderType(v as "market" | "limit")} options={[{ v: "market", l: "Market" }, { v: "limit", l: "Limit" }]} />
          <div className="mt-3">
            <Segmented
              value={isLong ? "long" : "short"}
              onChange={(v) => setIsLong(v === "long")}
              options={[{ v: "long", l: "Long ▲", color: "up" }, { v: "short", l: "Short ▼", color: "down" }]}
            />
          </div>

          <div className="mt-4">
            <label className="text-[10px] font-medium uppercase tracking-[0.16em] text-soft">Collateral (mUSDC)</label>
            <input value={collateral} onChange={(e) => setCollateral(e.target.value)} inputMode="decimal" className="mt-1.5 w-full rounded-lg border border-border bg-background px-3 py-2.5 font-mono text-sm tabular-nums outline-none focus:border-accent" />
            <div className="mt-1 flex justify-between text-[11px] text-soft">
              <span>Wallet: {bal ? fmtUsdc(bal.usdc) : "—"} mUSDC</span>
              {bal && <button className="text-accent hover:underline" onClick={() => setCollateral(fmtUsdc(bal.usdc).replace(/,/g, ""))}>Max</button>}
            </div>
          </div>

          {orderType === "limit" && (
            <div className="mt-4">
              <label className="text-[10px] font-medium uppercase tracking-[0.16em] text-soft">Trigger price ({index.toUpperCase()})</label>
              <input value={trigger} onChange={(e) => setTrigger(e.target.value)} inputMode="decimal" placeholder={markVol !== null ? markVol.toFixed(2) : "—"} className="mt-1.5 w-full rounded-lg border border-border bg-background px-3 py-2.5 font-mono text-sm tabular-nums outline-none focus:border-accent" />
              {triggerValid && triggerUnits > 0n && markVol !== null && (
                <div className="mt-1 text-[11px] text-soft">Fires when price {triggerAbove ? "rises to ≥" : "falls to ≤"} {triggerVol.toFixed(2)}</div>
              )}
            </div>
          )}

          <div className="mt-4">
            <div className="flex items-center justify-between">
              <label className="text-[10px] font-medium uppercase tracking-[0.16em] text-soft">Leverage</label>
              <span className="font-mono text-sm font-semibold text-accent">{leverage}×</span>
            </div>
            <input type="range" min={1} max={MAX_LEVERAGE} value={leverage} onChange={(e) => setLeverage(Number(e.target.value))} className="mt-2 w-full accent-[var(--accent)]" />
          </div>

          <div className="mt-4 grid grid-cols-3 gap-3 rounded-lg border border-border-subtle bg-background/40 p-3">
            <Stat label="Notional" value={fmtUsdc(notional)} />
            <Stat label={`${index.toUpperCase()} mark`} value={markVol !== null ? markVol.toFixed(2) : "—"} accent="accent" />
            <Stat label="Est. liq" value={previewLiq !== null ? previewLiq.toFixed(2) : "—"} accent="down" />
          </div>
          <div className="mt-2 text-[11px] text-soft">Borrow fee: {fundingPctDay.toFixed(2)}% / day on notional</div>

          {priceUnset && <p className="mt-3 text-xs text-down">Oracle price unset — keeper offline.</p>}
          {priceStale && <p className="mt-3 text-xs text-down">Oracle price stale (&gt; 1h). Wait for the keeper.</p>}

          <div className="mt-4 flex flex-col gap-2">
            {needsApproval ? (
              <TxButton label={`Approve ${collateral} mUSDC`} onRun={approve} disabled={collateralUnits === 0n} />
            ) : orderType === "market" ? (
              <TxButton label={`Open ${isLong ? "long" : "short"} ${leverage}×`} variant={isLong ? "up" : "down"} onRun={open} disabled={!marketCanOpen || collateralUnits === 0n || (!!bal && collateralUnits > bal.usdc)} />
            ) : (
              <TxButton label={`Place limit ${isLong ? "long" : "short"}`} variant={isLong ? "up" : "down"} onRun={placeLimit} disabled={!triggerValid || triggerUnits === 0n || collateralUnits === 0n || (!!bal && collateralUnits > bal.usdc)} />
            )}
          </div>
        </Card>

        <Card title="Faucet">
          <p className="text-xs text-muted">Need test collateral? Mint 10,000 mUSDC.</p>
          <div className="mt-3"><TxButton label="Claim 10,000 mUSDC" onRun={faucet} /></div>
        </Card>
      </div>

      {/* Positions + orders */}
      <div className="flex flex-col gap-4">
        <Card title="Your positions">
          {positions.length === 0 ? (
            <p className="py-6 text-center text-sm text-soft">No open positions.</p>
          ) : (
            <div className="flex flex-col divide-y divide-border-subtle">
              {positions.map((p) => (
                <PositionRow key={p.id.toString()} p={p} mark={prices[p.index]} onClose={() => close(p.id)} onStop={placeStop} />
              ))}
            </div>
          )}
        </Card>

        <Card title="Open orders">
          {orders.length === 0 ? (
            <p className="py-6 text-center text-sm text-soft">No pending orders.</p>
          ) : (
            <div className="flex flex-col divide-y divide-border-subtle">
              {orders.map((o) => (
                <OrderRow key={o.id.toString()} o={o} onCancel={() => cancel(o.id)} />
              ))}
            </div>
          )}
        </Card>
      </div>
    </div>
  );
}

function PositionRow({
  p,
  mark,
  onClose,
  onStop,
}: {
  p: UserPosition;
  mark: OraclePrice | null;
  onClose: () => Promise<void>;
  onStop: (positionId: bigint, triggerPrice: bigint, above: boolean, takeProfit: boolean) => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [tp, setTp] = useState("");
  const [sl, setSl] = useState("");
  const win = p.pnl >= 0n;
  const entryVol = toVol(p.entryPrice);
  const liq = liqPriceVol(entryVol, Number(p.leverage), p.isLong);
  const markVol = mark && mark.value > 0n ? toVol(mark.value) : null;
  const distPct = markVol !== null && markVol > 0 ? (Math.abs(markVol - liq) / markVol) * 100 : null;
  const near = distPct !== null && distPct < 10;

  // TP/SL trigger direction from side: long TP above + SL below; short reversed.
  const submitStop = (raw: string, takeProfit: boolean) => {
    const v = Number(raw);
    if (!Number.isFinite(v) || v <= 0) return Promise.resolve();
    const above = p.isLong === takeProfit; // long&TP / short&SL fire above
    return onStop(p.id, BigInt(Math.round(v * VALUE_SCALE)), above, takeProfit);
  };

  return (
    <div className="py-3">
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <span className={`rounded-md px-2 py-0.5 text-[10px] font-bold uppercase ${p.isLong ? "bg-up-soft text-up" : "bg-down-soft text-down"}`}>
            {p.isLong ? "Long" : "Short"} {Number(p.leverage)}×
          </span>
          <div>
            <div className="text-sm font-semibold">{p.index.toUpperCase()}</div>
            <div className="font-mono text-[11px] text-soft">
              entry {fmtPrice(p.entryPrice)} · liq {liq.toFixed(2)}
              {distPct !== null && <span className={near ? "text-down" : "text-soft"}> · {distPct.toFixed(1)}% away</span>}
              {" · fee "}{fmtUsdc(p.funding)}
            </div>
          </div>
        </div>
        <div className="flex items-center gap-3">
          <div className="text-right">
            <div className={`font-mono text-sm font-semibold tabular-nums ${win ? "text-up" : "text-down"}`}>{fmtPnl(p.pnl)}</div>
            <div className="font-mono text-[11px] text-soft">eq {fmtUsdc(p.equity)}</div>
          </div>
          {p.liquidatable && <span className="rounded bg-down-soft px-1.5 py-0.5 text-[10px] font-bold text-down">LIQ</span>}
          <button onClick={() => setOpen((o) => !o)} className="rounded-lg border border-border px-2 py-1.5 text-[11px] font-medium text-muted hover:border-accent hover:text-accent">
            TP/SL
          </button>
          <CloseButton onClose={onClose} />
        </div>
      </div>

      {open && (
        <div className="mt-3 grid grid-cols-2 gap-3 rounded-lg border border-border-subtle bg-background/40 p-3">
          <StopField label="Take profit" placeholder="price" value={tp} setValue={setTp} variant="up" onSet={() => submitStop(tp, true)} />
          <StopField label="Stop loss" placeholder="price" value={sl} setValue={setSl} variant="down" onSet={() => submitStop(sl, false)} />
        </div>
      )}
    </div>
  );
}

function StopField({
  label,
  placeholder,
  value,
  setValue,
  variant,
  onSet,
}: {
  label: string;
  placeholder: string;
  value: string;
  setValue: (s: string) => void;
  variant: "up" | "down";
  onSet: () => Promise<void>;
}) {
  return (
    <div>
      <label className={`text-[10px] font-medium uppercase tracking-[0.14em] ${variant === "up" ? "text-up" : "text-down"}`}>{label}</label>
      <input value={value} onChange={(e) => setValue(e.target.value)} inputMode="decimal" placeholder={placeholder} className="mt-1 w-full rounded-md border border-border bg-background px-2 py-1.5 font-mono text-xs tabular-nums outline-none focus:border-accent" />
      <div className="mt-1.5">
        <TxButton label={`Set ${label.split(" ")[0]}`} variant={variant} onRun={onSet} disabled={!value.trim()} />
      </div>
    </div>
  );
}

function OrderRow({ o, onCancel }: { o: OrderItem; onCancel: () => Promise<void> }) {
  const kindLabel = o.kind === "limit" ? `Limit ${o.isLong ? "long" : "short"} ${Number(o.leverage)}×` : o.kind === "tp" ? "Take profit" : "Stop loss";
  const color = o.kind === "tp" ? "text-up" : o.kind === "sl" ? "text-down" : "text-accent";
  return (
    <div className="flex items-center justify-between gap-3 py-3">
      <div>
        <div className={`text-sm font-semibold ${color}`}>{kindLabel}</div>
        <div className="font-mono text-[11px] text-soft">
          {o.index.toUpperCase()} · {o.triggerAbove ? "≥" : "≤"} {fmtPrice(o.triggerPrice)}
          {o.kind === "limit" && <> · {fmtUsdc(o.collateral)} mUSDC</>}
        </div>
      </div>
      <CloseButton onClose={onCancel} label="Cancel" />
    </div>
  );
}

function CloseButton({ onClose, label = "Close" }: { onClose: () => Promise<void>; label?: string }) {
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
        {busy ? "…" : label}
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
          <button key={o.v} onClick={() => onChange(o.v)} className={`rounded-md px-3 py-2 text-sm font-semibold transition-colors ${active ? activeColor : "text-muted hover:text-foreground"}`}>
            {o.l}
          </button>
        );
      })}
    </div>
  );
}
