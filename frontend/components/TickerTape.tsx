"use client";

import { useIndexTicks } from "@/lib/useIndexTicks";
import type { IndexId } from "@/lib/api";

// Scrolling marquee under the hero. Reads as "live wire" — the items
// loop continuously even when the WS state is degraded, but the values
// shown drop to "—" until a real tick arrives. Two copies of the items
// concatenated so the animation can loop seamlessly (CSS marquee
// pattern; no JS frame loop).

const CHANNELS: IndexId[] = ["bvol", "evol"];

interface ItemProps {
  k: string;
  v: string;
  tone?: "accent" | "up" | "down" | "default";
}

function Item({ k, v, tone = "default" }: ItemProps) {
  const toneCls =
    tone === "accent"
      ? "text-accent"
      : tone === "up"
        ? "text-up"
        : tone === "down"
          ? "text-down"
          : "text-foreground";
  return (
    <span className="flex items-baseline gap-2 whitespace-nowrap px-6">
      <span className="text-[10px] font-medium uppercase tracking-[0.22em] text-soft">{k}</span>
      <span className={`font-mono text-sm font-semibold tabular-nums ${toneCls}`}>{v}</span>
    </span>
  );
}

function Sep() {
  return <span className="inline-block h-1 w-1 rounded-full bg-soft-2" aria-hidden />;
}

export function TickerTape() {
  const { state, ticks } = useIndexTicks(CHANNELS);

  const bvol = ticks.bvol;
  const evol = ticks.evol;

  const items: ItemProps[] = [
    { k: "BVOL", v: bvol ? bvol.value.toFixed(2) : "—", tone: "accent" },
    { k: "EVOL", v: evol ? evol.value.toFixed(2) : "—", tone: "accent" },
    {
      k: "venues",
      v: state === "open" ? "3 / 3 live" : state === "connecting" ? "connecting" : "offline",
    },
    { k: "cadence", v: "60s" },
    { k: "tenor", v: "30d" },
    {
      k: "conf",
      v:
        bvol?.confidence !== undefined
          ? bvol.confidence.toFixed(2)
          : evol?.confidence !== undefined
            ? evol.confidence.toFixed(2)
            : "—",
    },
    { k: "methodology", v: "VIX-family" },
    { k: "source", v: "open" },
  ];

  // Render the items twice so the marquee loop is seamless.
  const renderItems = (key: string) => (
    <div key={key} className="flex shrink-0 items-center gap-3">
      {items.map((it, i) => (
        <span key={`${key}-${i}`} className="flex items-center gap-3">
          <Item {...it} />
          {i < items.length - 1 && <Sep />}
        </span>
      ))}
      <span className="px-6"><Sep /></span>
    </div>
  );

  return (
    <div className="border-y border-border-subtle bg-surface/60 backdrop-blur-sm">
      <div className="volx-marquee py-3">
        {renderItems("a")}
        {renderItems("b")}
      </div>

      <style jsx>{`
        .volx-marquee {
          display: flex;
          width: max-content;
          animation: volx-marquee 60s linear infinite;
        }
        @keyframes volx-marquee {
          from {
            transform: translateX(0);
          }
          to {
            transform: translateX(-50%);
          }
        }
        .volx-marquee:hover {
          animation-play-state: paused;
        }
      `}</style>
    </div>
  );
}
