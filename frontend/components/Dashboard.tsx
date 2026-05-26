"use client";

import { useEffect, useState } from "react";
import { fetchHistory, fetchLatest, type IndexId } from "@/lib/api";
import { useIndexTicks, type ConnState } from "@/lib/useIndexTicks";
import { IndexCard } from "./IndexCard";

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
    <section className="mx-auto w-full max-w-3xl px-6">
      <header className="mb-10 text-center">
        <h1 className="text-4xl font-semibold tracking-tight">VolX</h1>
        <p className="mt-2 text-sm text-foreground/60">
          Crypto volatility index. 60-second cadence, multi-venue blend.
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
      <p className="mt-3 text-xs text-rose-400/80">
        api unreachable — start the local pipeline (`docker compose up` + engine).
      </p>
    );
  }
  const label = state === "open" ? "live" : state === "connecting" ? "connecting" : "reconnecting";
  const color =
    state === "open" ? "bg-emerald-400" : state === "connecting" ? "bg-amber-400" : "bg-rose-400";
  return (
    <div className="mt-3 inline-flex items-center gap-2 text-[11px] uppercase tracking-widest text-foreground/40">
      <span className={`inline-block h-1.5 w-1.5 rounded-full ${color}`} />
      {label}
    </div>
  );
}
