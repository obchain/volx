"use client";

import { useEffect, useState } from "react";
import { fetchHistory, fetchLatest, type IndexId } from "@/lib/api";
import { useIndexTicks } from "@/lib/useIndexTicks";
import { IndexCard } from "./IndexCard";
import { LivePulse } from "./LivePulse";

interface IndexBootstrap {
  initialValue: number | null;
  initialTs: string | null;
  history: number[];
}

const EMPTY: IndexBootstrap = { initialValue: null, initialTs: null, history: [] };

// 1h sparkline at 5m candles → 12 points
const HISTORY_INTERVAL = "5m" as const;
const HISTORY_LIMIT = 12;
const CHANNELS: IndexId[] = ["bvol", "evol"];

export function Dashboard() {
  const { state, ticks } = useIndexTicks(CHANNELS);
  const [bvol, setBvol] = useState<IndexBootstrap>(EMPTY);
  const [evol, setEvol] = useState<IndexBootstrap>(EMPTY);
  const [bootErr, setBootErr] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      try {
        const [latestBvol, histBvol, latestEvol, histEvol] = await Promise.all([
          fetchLatest("bvol"),
          fetchHistory("bvol", HISTORY_INTERVAL, HISTORY_LIMIT),
          fetchLatest("evol"),
          fetchHistory("evol", HISTORY_INTERVAL, HISTORY_LIMIT),
        ]);
        if (cancelled) return;
        setBvol({
          initialValue: latestBvol.value,
          initialTs: latestBvol.ts,
          history: histBvol.bars.map((b) => b.close),
        });
        setEvol({
          initialValue: latestEvol.value,
          initialTs: latestEvol.ts,
          history: histEvol.bars.map((b) => b.close),
        });
      } catch (e) {
        if (!cancelled) setBootErr(e instanceof Error ? e.message : String(e));
      }
    };
    run();
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <section className="mx-auto w-full max-w-5xl px-6 pt-16 sm:pt-24">
      {/* Hero copy */}
      <div className="flex flex-col items-center gap-5 text-center">
        <span className="inline-flex items-center gap-2 rounded-full border border-accent/30 bg-accent-soft px-3 py-1 text-[10px] font-medium uppercase tracking-[0.22em] text-accent">
          <span className="h-1 w-1 rounded-full bg-accent volx-pulse" />
          open crypto volatility index
        </span>
        <h1 className="max-w-3xl text-5xl font-semibold leading-[1.05] tracking-tight text-foreground sm:text-6xl md:text-7xl">
          The open
          <br />
          <span className="text-accent">crypto volatility</span> index.
        </h1>
        <p className="max-w-xl text-base text-muted sm:text-lg">
          BVOL + EVOL — 30-day implied volatility for Bitcoin and Ethereum, blended across{" "}
          <span className="font-mono text-foreground">deribit</span>,{" "}
          <span className="font-mono text-foreground">okx</span>, and{" "}
          <span className="font-mono text-foreground">bybit</span>. Published every 60 seconds.
          Methodology open. Self-hostable.
        </p>
        <div className="mt-2 flex items-center gap-3">
          <LivePulse state={state} />
          {bootErr && (
            <span className="text-xs text-down/85">
              api unreachable — start the local pipeline.
            </span>
          )}
        </div>
      </div>

      {/* Hero index cards */}
      <div className="mt-12 grid gap-5 sm:grid-cols-2">
        <IndexCard
          id="bvol"
          liveTick={ticks.bvol}
          initialValue={bvol.initialValue}
          initialTs={bvol.initialTs}
          history={bvol.history}
        />
        <IndexCard
          id="evol"
          liveTick={ticks.evol}
          initialValue={evol.initialValue}
          initialTs={evol.initialTs}
          history={evol.history}
        />
      </div>
    </section>
  );
}
