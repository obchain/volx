"use client";

import Link from "next/link";
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

const LABEL: Record<IndexId, string> = {
  bvol: "Bitcoin Volatility",
  evol: "Ethereum Volatility",
};

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
    <Link
      href={`/chart/${id}`}
      className="group relative block overflow-hidden rounded-2xl border border-border-subtle bg-surface p-6 transition-colors hover:border-border-strong"
    >
      <div className="flex items-baseline justify-between">
        <div className="flex items-baseline gap-2">
          <span className="text-xs font-medium uppercase tracking-[0.2em] text-foreground">
            {id}
          </span>
          <span className="text-[10px] text-soft-2">{LABEL[id]}</span>
        </div>
        <span className="text-[10px] uppercase tracking-wider text-soft">
          {confidence !== null ? `c ${confidence.toFixed(2)}` : "—"}
        </span>
      </div>

      <div className="mt-4 flex items-baseline gap-3">
        <span className="text-5xl font-semibold tabular-nums tracking-tight">
          {value !== null ? value.toFixed(2) : "—"}
        </span>
        {deltaPct !== null && (
          <span
            className={
              tone === "up"
                ? "text-sm font-medium text-up"
                : tone === "down"
                  ? "text-sm font-medium text-down"
                  : "text-sm text-soft"
            }
          >
            {deltaPct >= 0 ? "+" : ""}
            {deltaPct.toFixed(2)}%<span className="ml-1 text-soft-2">1h</span>
          </span>
        )}
      </div>

      <div className="mt-5">
        <Sparkline values={history} className="w-full" height={64} />
      </div>

      <div className="mt-4 flex items-center justify-between text-[11px]">
        <span className="text-soft">{tsMs ? new Date(tsMs).toLocaleTimeString() : "no data"}</span>
        <span className="text-soft transition-colors group-hover:text-foreground">chart →</span>
      </div>
    </Link>
  );
}
