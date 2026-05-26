"use client";

import { Sparkline } from "./Sparkline";
import type { IndexId } from "@/lib/api";
import type { Tick } from "@/lib/useIndexTicks";

interface IndexCardProps {
  id: IndexId;
  liveTick: Tick | null;
  initialValue: number | null;
  initialTs: string | null;
  history: number[];
}

export function IndexCard({ id, liveTick, initialValue, initialTs, history }: IndexCardProps) {
  const value = liveTick?.value ?? initialValue;
  const tsMs = liveTick?.ts ?? (initialTs ? Date.parse(initialTs) : null);
  const confidence = liveTick?.confidence ?? null;

  const first: number | undefined = history[0];
  const last: number | undefined = history[history.length - 1];
  const delta = first !== undefined && last !== undefined ? last - first : null;
  const deltaPct =
    first !== undefined && first !== 0 && delta !== null ? (delta / first) * 100 : null;

  const tone = deltaPct === null ? "neutral" : deltaPct >= 0 ? "up" : "down";

  return (
    <div className="rounded-2xl border border-white/10 bg-white/[0.02] p-6 backdrop-blur-sm">
      <div className="flex items-baseline justify-between">
        <span className="text-xs uppercase tracking-widest text-foreground/50">{id}</span>
        <span className="text-[10px] uppercase tracking-wider text-foreground/40">
          {confidence !== null ? `confidence ${confidence.toFixed(2)}` : "—"}
        </span>
      </div>

      <div className="mt-3 flex items-baseline gap-3">
        <span className="text-5xl font-semibold tabular-nums tracking-tight">
          {value !== null ? value.toFixed(2) : "—"}
        </span>
        {deltaPct !== null && (
          <span
            className={
              tone === "up"
                ? "text-sm text-emerald-400"
                : tone === "down"
                  ? "text-sm text-rose-400"
                  : "text-sm text-foreground/40"
            }
          >
            {deltaPct >= 0 ? "+" : ""}
            {deltaPct.toFixed(2)}% 1h
          </span>
        )}
      </div>

      <div className="mt-4">
        <Sparkline values={history} className="w-full" />
      </div>

      <div className="mt-3 text-[11px] text-foreground/40">
        {tsMs ? `last tick ${new Date(tsMs).toLocaleTimeString()}` : "no data"}
      </div>
    </div>
  );
}
