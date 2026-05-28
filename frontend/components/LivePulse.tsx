"use client";

import type { ConnState } from "@/lib/useIndexTicks";

interface LivePulseProps {
  state: ConnState;
  label?: boolean;
  size?: "sm" | "md";
}

// Live connection indicator. A coloured dot + optional state label.
// State drives the colour: open → cyan-accent + pulse, connecting →
// amber + pulse, closed → soft.
export function LivePulse({ state, label = true, size = "sm" }: LivePulseProps) {
  const dot = size === "md" ? "h-2 w-2" : "h-1.5 w-1.5";
  const colorCls =
    state === "open"
      ? "bg-accent shadow-[0_0_8px_var(--accent-glow)]"
      : state === "connecting"
        ? "bg-amber-400"
        : "bg-soft-2";

  const stateText =
    state === "open" ? "live" : state === "connecting" ? "connecting" : "offline";

  return (
    <span className="inline-flex items-center gap-2 text-[10px] font-medium uppercase tracking-[0.18em] text-soft">
      <span className="relative inline-flex">
        <span className={`relative inline-block rounded-full ${dot} ${colorCls}`} />
        {state === "open" && (
          <span
            aria-hidden
            className={`volx-pulse absolute inset-0 rounded-full ${dot} bg-accent opacity-60`}
          />
        )}
      </span>
      {label && <span>{stateText}</span>}
    </span>
  );
}
