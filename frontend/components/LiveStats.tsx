"use client";

import { useEffect, useState } from "react";

// Pyth-style stats anchor. Sits below the hero IndexCards and signals
// scale + cadence + transparency without overstating: the numbers we
// publish here are facts about the methodology, not derived metrics
// that depend on per-tick API responses. Keeps the component cheap and
// truthful — no risk of showing 0/3 venues live mid-API-warmup.

interface StatProps {
  k: string;
  v: string;
  hint?: string;
}

function Stat({ k, v, hint }: StatProps) {
  return (
    <div className="flex flex-col gap-1">
      <span className="font-mono text-2xl font-semibold tabular-nums text-foreground">{v}</span>
      <span className="text-[10px] font-medium uppercase tracking-[0.18em] text-soft">{k}</span>
      {hint && <span className="text-[10px] text-soft-2">{hint}</span>}
    </div>
  );
}

export function LiveStats() {
  // Tick a once-a-second clock so the "last update" relative time stays
  // honest. Keeping it inside the component means LiveStats can be
  // dropped onto any page without prop wiring.
  const [now, setNow] = useState<number>(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  // Cosmetic uptime stamp — measures how long the page has been open,
  // not the engine's uptime. Honest framing in the hint avoids misleading
  // a casual reader.
  const [mountedAt] = useState<number>(() => Date.now());
  const sec = Math.max(0, Math.floor((now - mountedAt) / 1000));

  return (
    <section className="mx-auto w-full max-w-5xl px-6">
      <div className="grid grid-cols-2 gap-6 rounded-2xl border border-border-subtle bg-surface px-8 py-6 sm:grid-cols-4">
        <Stat k="venues" v="3" hint="deribit · okx · bybit" />
        <Stat k="cadence" v="60s" hint="every minute" />
        <Stat k="tenor" v="30d" hint="constant maturity" />
        <Stat k="methodology" v="VIX-family" hint="open · auditable" />
      </div>
      <div className="mt-4 flex flex-wrap items-center justify-between gap-3 px-2 text-[11px] text-soft">
        <span>
          built on <span className="font-mono">deribit</span> ·{" "}
          <span className="font-mono">okx</span> · <span className="font-mono">bybit</span> ·
          median blend
        </span>
        <span className="font-mono tabular-nums">
          {fmtUptime(sec)} this session
        </span>
      </div>
    </section>
  );
}

function fmtUptime(sec: number): string {
  if (sec < 60) return `${sec}s`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m ${sec % 60}s`;
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  return `${h}h ${m}m`;
}
