import Link from "next/link";

// Trust strip — three short cards that anchor the differentiators
// against the two competing crypto vol indices (Deribit DVOL, Volmex
// BVIV / EVIV). Lives below the LiveStats anchor on the landing page.

interface Card {
  k: string;
  v: string;
  body: string;
}

const CARDS: Card[] = [
  {
    k: "3 venues, with quorum",
    v: "deribit · okx · bybit",
    body:
      "Per-venue strip + median blend. If a venue's number diverges by 5% for 5 consecutive ticks, it gets dropped — and the remaining two still constitute a quorum. DVOL (1 venue) and BVIV (2 venues) cannot do this.",
  },
  {
    k: "confidence per tick",
    v: "venue × fresh × strikes",
    body:
      "Every published tick carries a [0, 1] confidence score that collapses when venues are missing, quotes are stale, or the strike range is thin. Downstream consumers can filter, gate, or weight by it.",
  },
  {
    k: "open + self-hostable",
    v: "docker compose up",
    body:
      "Source, methodology, validation procedure — all public. Runs locally on a laptop with one command. No paid feed, no API key gate, no cloud dependency.",
  },
];

export function TrustStrip() {
  return (
    <section className="mx-auto w-full max-w-5xl px-6">
      <div className="mb-8 flex items-end justify-between gap-6">
        <h2 className="max-w-md text-2xl font-semibold tracking-tight text-foreground sm:text-3xl">
          What makes <span className="text-accent">VolX</span> different.
        </h2>
        <Link
          href="https://github.com/obchain/volx/blob/main/METHODOLOGY.md"
          target="_blank"
          rel="noreferrer"
          className="hidden text-xs font-medium text-muted underline-offset-4 transition-colors hover:text-accent hover:underline sm:inline"
        >
          read the methodology →
        </Link>
      </div>
      <div className="grid gap-4 sm:grid-cols-3">
        {CARDS.map((c) => (
          <article
            key={c.k}
            className="flex flex-col gap-3 rounded-2xl border border-border-subtle bg-surface p-6 transition-colors hover:border-accent/40"
          >
            <span className="text-[10px] font-medium uppercase tracking-[0.18em] text-accent">
              {c.k}
            </span>
            <span className="font-mono text-sm font-semibold text-foreground">{c.v}</span>
            <p className="text-xs leading-relaxed text-muted">{c.body}</p>
          </article>
        ))}
      </div>
    </section>
  );
}
