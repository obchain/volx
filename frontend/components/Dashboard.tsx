"use client";

import { useEffect, useState } from "react";
import { fetchHistory, fetchLatest, type IndexId } from "@/lib/api";
import { useIndexTicks, type ConnState } from "@/lib/useIndexTicks";
import { IndexCard } from "./IndexCard";
import { ThemeToggle } from "./ThemeToggle";

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
    <section className="mx-auto w-full max-w-4xl px-6">
      <div className="absolute top-4 right-4">
        <ThemeToggle />
      </div>
      <header className="mb-12 flex flex-col items-center text-center">
        <span className="rounded-full border border-border-subtle bg-surface px-3 py-1 text-[10px] uppercase tracking-[0.25em] text-soft">
          crypto volatility index
        </span>
        <h1 className="mt-5 text-6xl font-semibold tracking-tight">VolX</h1>
        <p className="mt-3 max-w-md text-sm text-muted">
          BVOL + EVOL — 30-day implied volatility for BTC and ETH, computed every 60 seconds from
          multi-venue options data.
        </p>
        <ConnStatus state={state} bootErr={bootErr} />
      </header>

      <div className="grid gap-5 sm:grid-cols-2">
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

function ConnStatus({ state, bootErr }: { state: ConnState; bootErr: string | null }) {
  if (bootErr) {
    return (
      <p className="mt-3 text-xs text-down/80">
        api unreachable — start the local pipeline (`docker compose up` + engine).
      </p>
    );
  }
  const label = state === "open" ? "live" : state === "connecting" ? "connecting" : "reconnecting";
  const dotCls = state === "open" ? "bg-up" : "bg-amber-400";
  return (
    <div className="mt-3 inline-flex items-center gap-2 text-[11px] uppercase tracking-widest text-soft">
      <span className={`inline-block h-1.5 w-1.5 rounded-full ${dotCls}`} />
      {label}
    </div>
  );
}
