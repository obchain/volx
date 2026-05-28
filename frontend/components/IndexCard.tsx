"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";
import { Sparkline } from "./Sparkline";
import { ConfidenceRing } from "./ConfidenceRing";
import type { IndexId } from "@/lib/api";
import type { Tick } from "@/lib/useIndexTicks";

interface IndexCardProps {
  id: IndexId;
  liveTick: Tick | null;
  initialValue: number | null;
  initialTs: string | null;
  history: number[];
}

const LABEL: Record<IndexId, { full: string; asset: string }> = {
  bvol: { full: "Bitcoin Volatility", asset: "BTC" },
  evol: { full: "Ethereum Volatility", asset: "ETH" },
};

// Detect a new WS tick and pulse the big number for ~750ms. The flash
// direction (up/down) is derived from the change vs the previous tick
// so the colour cue reinforces direction.
function useTickFlash(value: number | null) {
  const [tone, setTone] = useState<"up" | "down" | null>(null);
  const prevRef = useRef<number | null>(null);

  useEffect(() => {
    if (value === null) return;
    const prev = prevRef.current;
    prevRef.current = value;
    if (prev === null || prev === value) return;
    setTone(value > prev ? "up" : "down");
    const t = window.setTimeout(() => setTone(null), 750);
    return () => window.clearTimeout(t);
  }, [value]);

  return tone;
}

// Pointer-tracked accent glow that follows the cursor across the card.
// Implemented as a CSS custom property update on the host element; no
// re-render churn, no React state for the position.
function usePointerGlow<T extends HTMLElement>() {
  const ref = useRef<T | null>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const move = (e: PointerEvent) => {
      const rect = el.getBoundingClientRect();
      el.style.setProperty("--glow-x", `${e.clientX - rect.left}px`);
      el.style.setProperty("--glow-y", `${e.clientY - rect.top}px`);
    };
    el.addEventListener("pointermove", move);
    return () => el.removeEventListener("pointermove", move);
  }, []);
  return ref;
}

export function IndexCard({ id, liveTick, initialValue, initialTs, history }: IndexCardProps) {
  const value = liveTick?.value ?? initialValue;
  const tsMs = liveTick?.ts ?? (initialTs ? Date.parse(initialTs) : null);
  const confidence = liveTick?.confidence ?? null;
  const flashTone = useTickFlash(liveTick?.value ?? null);
  const ref = usePointerGlow<HTMLAnchorElement>();

  const first: number | undefined = history[0];
  const last: number | undefined = history[history.length - 1];
  const delta = first !== undefined && last !== undefined ? last - first : null;
  const deltaPct =
    first !== undefined && first !== 0 && delta !== null ? (delta / first) * 100 : null;

  const tone = deltaPct === null ? "neutral" : deltaPct >= 0 ? "up" : "down";
  const meta = LABEL[id];
  const isLive = liveTick !== null;

  return (
    <Link
      ref={ref}
      href={`/chart/${id}`}
      className="group volx-glow-card relative block overflow-hidden rounded-2xl border border-border-subtle bg-surface p-7 transition-all hover:border-accent/40 hover:bg-surface-2 hover:shadow-[0_8px_32px_-12px_var(--accent-glow)]"
    >
      {/* Cursor-tracking accent glow layer */}
      <span aria-hidden className="volx-glow-layer" />

      {/* Top row: label + asset + confidence ring */}
      <div className="relative flex items-start justify-between">
        <div className="flex flex-col gap-1.5">
          <div className="flex items-baseline gap-2">
            <span className="text-sm font-semibold uppercase tracking-[0.2em] text-foreground">
              {id}
            </span>
            <span className="text-[10px] uppercase tracking-[0.18em] text-soft">
              {meta.asset}
            </span>
            {isLive && (
              <span
                aria-hidden
                className="volx-pulse ml-1 h-1.5 w-1.5 rounded-full bg-accent"
              />
            )}
          </div>
          <span className="text-xs text-soft">{meta.full}</span>
        </div>
        <ConfidenceRing value={confidence} size={52} thickness={3.5} />
      </div>

      {/* Big value + delta */}
      <div className="relative mt-6 flex items-baseline gap-4">
        <span
          className={`volx-flash font-mono text-7xl font-semibold tabular-nums leading-none tracking-tight ${
            flashTone === "up"
              ? "text-up volx-flash-active"
              : flashTone === "down"
                ? "text-down volx-flash-active"
                : "text-foreground"
          }`}
        >
          {value !== null ? value.toFixed(2) : "—"}
        </span>
        {deltaPct !== null && (
          <div className="flex flex-col gap-0.5">
            <span
              className={`font-mono text-sm font-semibold tabular-nums ${
                tone === "up" ? "text-up" : tone === "down" ? "text-down" : "text-soft"
              }`}
            >
              {deltaPct >= 0 ? "+" : ""}
              {deltaPct.toFixed(2)}%
            </span>
            <span className="text-[10px] uppercase tracking-[0.18em] text-soft-2">1h</span>
          </div>
        )}
      </div>

      {/* Sparkline */}
      <div className="relative mt-6">
        <Sparkline values={history} className="w-full" height={68} tone="accent" />
      </div>

      {/* Footer row */}
      <div className="relative mt-5 flex items-center justify-between text-[11px]">
        <div className="flex items-center gap-3 text-soft">
          <span className="font-mono tabular-nums">
            {tsMs ? new Date(tsMs).toLocaleTimeString() : "no data"}
          </span>
          <span className="text-soft-2">·</span>
          <span>30d implied vol</span>
        </div>
        <span className="text-soft transition-colors group-hover:text-accent">
          open chart →
        </span>
      </div>

      <style jsx>{`
        :global(.volx-glow-card) {
          --glow-x: 50%;
          --glow-y: 50%;
        }
        :global(.volx-glow-layer) {
          position: absolute;
          inset: 0;
          background: radial-gradient(
            300px circle at var(--glow-x) var(--glow-y),
            var(--accent-glow),
            transparent 60%
          );
          opacity: 0;
          transition: opacity 220ms ease-out;
          pointer-events: none;
          z-index: 0;
        }
        :global(.volx-glow-card:hover .volx-glow-layer) {
          opacity: 1;
        }
        :global(.volx-flash) {
          transition: transform 600ms ease-out, color 600ms ease-out, text-shadow 600ms ease-out;
        }
        :global(.volx-flash-active) {
          transform: scale(1.04);
          text-shadow: 0 0 24px var(--accent-glow);
        }
      `}</style>
    </Link>
  );
}
