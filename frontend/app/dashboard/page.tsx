"use client";

import { useEffect, useState } from "react";
import { Header } from "@/components/Header";
import { Footer } from "@/components/Footer";
import { Card, Stat } from "@/components/dapp";
import { TradeChart } from "@/components/TradeChart";
import { IvSmileChart } from "@/components/IvSmileChart";
import { useWallet } from "@/lib/wallet";
import { useIndexTicks } from "@/lib/useIndexTicks";
import { fetchHealth, type HealthResponse, type IndexId } from "@/lib/api";
import { fmtUsdc } from "@/lib/format";
import { readProtocolStats, type ProtocolStats } from "@/lib/protocol";

const INDICES: IndexId[] = ["bvol", "evol"];

export default function DashboardPage() {
  const { publicClient } = useWallet();
  const { ticks } = useIndexTicks(INDICES);
  const [stats, setStats] = useState<ProtocolStats | null>(null);
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [smile, setSmile] = useState<IndexId>("bvol");

  useEffect(() => {
    let alive = true;
    const load = () => {
      readProtocolStats(publicClient).then((s) => alive && setStats(s)).catch(() => {});
      fetchHealth().then((h) => alive && setHealth(h)).catch(() => {});
    };
    load();
    const t = setInterval(load, 15_000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [publicClient]);

  const fresh = health ? health.last_update_age_s < 120 : false;

  return (
    <div className="flex min-h-dvh flex-col">
      <Header />
      <main className="mx-auto w-full max-w-6xl flex-1 px-5 py-8">
        {/* Title + live status strip */}
        <div className="mb-6 flex flex-wrap items-end justify-between gap-4">
          <div>
            <h1 className="text-2xl font-semibold tracking-tight">Dashboard</h1>
            <p className="mt-1 text-sm text-muted">
              Live BVOL &amp; EVOL, the implied-vol smile behind them, and the on-chain protocol they power.
            </p>
          </div>
          <div className="flex items-center gap-4 text-[11px] text-soft">
            <span className="inline-flex items-center gap-1.5">
              <span className={`h-1.5 w-1.5 rounded-full ${fresh ? "volx-pulse bg-up" : "bg-soft-2"}`} aria-hidden />
              {health ? `engine ${health.status} · ${health.last_update_age_s.toFixed(0)}s ago` : "engine —"}
            </span>
            <span className="text-soft-2">·</span>
            <span>Deribit · OKX · Bybit</span>
            {health && <span className="text-soft-2">· v{health.version}</span>}
          </div>
        </div>

        {/* Live index values */}
        <div className="mb-6 grid grid-cols-2 gap-4 sm:grid-cols-4">
          {INDICES.map((id) => {
            const tk = ticks[id];
            return (
              <Card key={id}>
                <div className="text-[10px] font-semibold uppercase tracking-[0.18em] text-accent">{id.toUpperCase()}</div>
                <div className="mt-1 font-mono text-3xl font-semibold tabular-nums">{tk ? tk.value.toFixed(2) : "—"}</div>
                <div className="mt-1 text-[11px] text-soft">
                  conf {tk ? `${(tk.confidence * 100).toFixed(0)}%` : "—"}
                </div>
              </Card>
            );
          })}
          <Card>
            <div className="text-[10px] font-semibold uppercase tracking-[0.18em] text-soft">Vault TVL</div>
            <div className="mt-1 font-mono text-3xl font-semibold tabular-nums text-accent">{stats ? fmtUsdc(stats.vault.totalAssets, 0) : "—"}</div>
            <div className="mt-1 text-[11px] text-soft">mUSDC</div>
          </Card>
          <Card>
            <div className="text-[10px] font-semibold uppercase tracking-[0.18em] text-soft">Open interest</div>
            <div className="mt-1 font-mono text-3xl font-semibold tabular-nums">{stats ? fmtUsdc(stats.openNotional, 0) : "—"}</div>
            <div className="mt-1 text-[11px] text-soft">{stats ? `${stats.openPositions} positions` : "—"}</div>
          </Card>
        </div>

        {/* Index history */}
        <div className="mb-6 grid gap-4 lg:grid-cols-2">
          <TradeChart id="bvol" />
          <TradeChart id="evol" />
        </div>

        {/* Protocol + IV smile */}
        <div className="grid gap-4 lg:grid-cols-2">
          <Card title="Protocol (on-chain)">
            <div className="grid grid-cols-2 gap-5 sm:grid-cols-3">
              <Stat label="TVL" value={stats ? fmtUsdc(stats.vault.totalAssets) : "—"} accent="accent" />
              <Stat label="Available" value={stats ? fmtUsdc(stats.vault.available) : "—"} />
              <Stat label="Reserved (OI)" value={stats ? fmtUsdc(stats.vault.reserved) : "—"} />
              <Stat label="Utilization" value={stats ? `${stats.utilizationPct.toFixed(1)}%` : "—"} accent={stats && stats.utilizationPct > 80 ? "down" : undefined} />
              <Stat label="Share price" value={stats ? stats.sharePrice.toFixed(6) : "—"} />
              <Stat label="Long / Short" value={stats ? `${stats.longs} / ${stats.shorts}` : "—"} />
            </div>
          </Card>

          <Card title="Implied-vol smile">
            <div className="mb-3 flex items-center justify-between">
              <span className="text-[11px] text-soft">Front-expiry strike → IV (the variance-integral input)</span>
              <div className="inline-flex rounded-lg border border-border-subtle bg-background/40 p-0.5 text-[11px]">
                {INDICES.map((id) => (
                  <button
                    key={id}
                    onClick={() => setSmile(id)}
                    className={smile === id ? "rounded-md bg-surface-2 px-2.5 py-1 text-foreground" : "rounded-md px-2.5 py-1 text-soft hover:text-foreground"}
                  >
                    {id.toUpperCase()}
                  </button>
                ))}
              </div>
            </div>
            <IvSmileChart id={smile} />
          </Card>
        </div>

        <p className="mt-6 text-[11px] text-soft">
          Index = 30-day implied vol, CBOE-style variance integral, median-blended across 3 venues. On-chain layer is a
          testnet demo (Sepolia) — not audited.
        </p>
      </main>
      <Footer />
    </div>
  );
}
