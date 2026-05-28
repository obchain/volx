import Link from "next/link";
import { Footer } from "@/components/Footer";
import { Header } from "@/components/Header";

export const metadata = {
  title: "Methodology — VolX",
  description:
    "How VolX computes BVOL and EVOL: per-venue VIX-family variance swap replication, median blend, outlier drop, confidence score.",
};

interface Section {
  k: string;
  title: string;
  body: React.ReactNode;
}

const SECTIONS: Section[] = [
  {
    k: "01",
    title: "Methodology base",
    body: (
      <>
        <p>
          VolX adopts the <strong className="text-foreground">Cboe VIX-family</strong> methodology
          — variance swap replication via a strip of out-of-the-money options — formalised in
          Cboe&apos;s 2003 white paper. Same maths family as the equity VIX, Deribit DVOL, and
          Volmex BVIV.
        </p>
        <p className="text-muted">What VolX computes per 60-second tick:</p>
        <ul className="ml-1 flex flex-col gap-1.5 text-muted">
          <li>
            <span className="text-accent">→</span> select two expiries bracketing 30 days (near + next)
          </li>
          <li>
            <span className="text-accent">→</span> solve the forward via put-call parity at the ATM strike
          </li>
          <li>
            <span className="text-accent">→</span> fit the IV surface as a natural cubic spline in log-moneyness
          </li>
          <li>
            <span className="text-accent">→</span> apply the Carr-Madan variance integral
          </li>
          <li>
            <span className="text-accent">→</span> interpolate in total-variance space to the exact 30-day point
          </li>
        </ul>
        <Formula>{`σ²_T  =  (2 e^{rT} / T) · ∫ Q(K) / K² dK  −  (F / K₀ − 1)² / T`}</Formula>
      </>
    ),
  },
  {
    k: "02",
    title: "Per-venue strip",
    body: (
      <>
        <p>
          The variance integral runs <strong className="text-foreground">independently per venue</strong>:
          one strip for Deribit, one for OKX, one for Bybit. Each venue produces its own σ²₃₀d
          number reflecting only its own order book.
        </p>
        <p className="text-muted">
          A venue&apos;s strip is rejected if it has fewer than 5 strikes with both call and put
          legs quoted &gt; 0.000000001 USD. Rejected venues are excluded from that 60-second tick
          and contribute to the confidence score collapse.
        </p>
      </>
    ),
  },
  {
    k: "03",
    title: "Median blend",
    body: (
      <>
        <p>
          The three venue values are combined by taking the{" "}
          <strong className="text-foreground">median</strong>, not the mean. A single bad venue
          cannot drag the median around the way it could the mean.
        </p>
        <Formula>{`BVOL  =  100 · √median(σ²_dervt,  σ²_okx,  σ²_bybit)`}</Formula>
        <p className="text-muted">
          For two surviving venues the median collapses to the simple mean of the two. For one
          surviving venue the value passes through unchanged. The published value is then 100 ·
          √σ²₃₀d — annualised vol percent.
        </p>
      </>
    ),
  },
  {
    k: "04",
    title: "Outlier drop",
    body: (
      <>
        <p>
          If a venue&apos;s σ²₃₀d deviates from the cross-venue median by more than{" "}
          <span className="font-mono text-accent">5%</span> for{" "}
          <span className="font-mono text-accent">5 consecutive ticks</span> (5 minutes), the
          engine drops that venue from the blend until its quotes return to consensus.
        </p>
        <p className="text-muted">
          <strong className="text-foreground">Availability rollback:</strong> if the drop would
          leave the active set empty (e.g. all three venues simultaneously diverging), the policy
          reverts — every venue is kept active and the confidence score absorbs the degraded
          state. The system never publishes a null for a transient quorum collapse.
        </p>
        <p className="text-muted">
          <strong className="text-foreground">Why three venues, not two:</strong> Volmex BVIV
          blends two venues (Deribit + OKX). With only two sources no real outlier policy is
          possible — drop one and you collapse to a single-venue index. VolX&apos;s third venue
          (Bybit) is specifically what makes the 5%/5-tick drop policy viable.
        </p>
      </>
    ),
  },
  {
    k: "05",
    title: "Confidence score",
    body: (
      <>
        <p>
          Every published index tick carries a{" "}
          <span className="font-mono text-accent">[0.0, 1.0]</span> confidence value computed from
          three multiplied factors:
        </p>
        <Formula>{`confidence  =  venue_factor  ×  freshness_factor  ×  strike_factor`}</Formula>
        <div className="flex flex-col gap-3">
          <Factor
            k="venue_factor"
            v="venues_live / venues_expected"
            note="e.g. 2/3 ≈ 0.667 if one venue is dropped"
          />
          <Factor
            k="freshness_factor"
            v="max(0, 1 − max_quote_age / 60s)"
            note="decays linearly as quotes age past 60 s"
          />
          <Factor
            k="strike_factor"
            v="min(1, strip_strikes / 8)"
            note="reaches 1 once the strip has ≥ 8 strikes"
          />
        </div>
        <p className="text-muted">
          Multiplicative so two simultaneous degradations compound rather than mask each other. A
          perfect snapshot (3/3 venues, all quotes fresh, ≥ 8 strikes) yields{" "}
          <span className="font-mono">confidence = 1.0</span>. Downstream consumers can filter,
          gate, or weight by the score.
        </p>
      </>
    ),
  },
  {
    k: "06",
    title: "Filters",
    body: (
      <>
        <p>Two layers of filtering protect the index from contaminated input.</p>
        <h4 className="text-sm font-semibold text-foreground">Normalizer layer (per quote)</h4>
        <ul className="ml-1 flex flex-col gap-1 text-muted">
          <li>· Drop if last tick &gt; 5 s old</li>
          <li>
            · Drop if <span className="font-mono">ask ≤ bid</span>
          </li>
          <li>
            · Drop if <span className="font-mono">(ask − bid) / mid &gt; 0.30</span>
          </li>
          <li>· Drop if mid &lt; intrinsic value (1e-9 tolerance)</li>
        </ul>
        <h4 className="text-sm font-semibold text-foreground">Engine layer (per snapshot)</h4>
        <ul className="ml-1 flex flex-col gap-1 text-muted">
          <li>
            · Drop strike if <span className="font-mono">mark_iv ≤ 0.001</span> or non-finite
          </li>
          <li>· Drop strike if either call or put leg is missing for that (strike, expiry)</li>
          <li>· Reject the expiry if fewer than 5 strikes survive</li>
        </ul>
      </>
    ),
  },
  {
    k: "07",
    title: "Relationship to Deribit DVOL",
    body: (
      <>
        <p>
          DVOL is the closest published reference: same VIX-family methodology, but on a
          single-venue (Deribit-only) input universe. Restricting VolX to Deribit-only input and
          running the engine should reproduce DVOL within numerical noise — the canonical
          correctness check.
        </p>
        <p className="text-muted">
          Acceptance bar per the PRD:{" "}
          <span className="font-mono text-foreground">|VolX − DVOL| / DVOL ≤ 2%</span> sustained
          over 30 consecutive days. Until that window passes, the index is shipped but not yet
          validated.
        </p>
      </>
    ),
  },
];

export default function MethodologyPage() {
  return (
    <main className="flex min-h-screen flex-col">
      <Header />
      <div className="flex-1">
        {/* Hero */}
        <section className="mx-auto w-full max-w-4xl px-6 pt-20 pb-12">
          <div className="flex flex-col items-center gap-5 text-center">
            <span className="inline-flex items-center gap-2 rounded-full border border-accent/30 bg-accent-soft px-3 py-1 text-[10px] font-medium uppercase tracking-[0.22em] text-accent">
              methodology
            </span>
            <h1 className="max-w-3xl text-4xl font-semibold leading-[1.05] tracking-tight text-foreground sm:text-5xl md:text-6xl">
              <span className="text-accent">VIX-family</span> maths,
              <br />
              every step open.
            </h1>
            <p className="max-w-2xl text-base text-muted">
              The same variance-swap-replication formula behind Cboe&apos;s VIX, Deribit&apos;s
              DVOL, and Volmex&apos;s BVIV — applied to a 3-venue crypto options universe with
              explicit median blending, outlier rejection, and a per-tick confidence score.
            </p>
            <div className="mt-2 flex flex-wrap items-center gap-3 text-[11px] text-soft">
              <a
                href="https://github.com/obchain/volx/blob/main/METHODOLOGY.md"
                target="_blank"
                rel="noreferrer"
                className="rounded-md border border-border-subtle bg-surface px-3 py-1.5 font-medium text-muted transition-colors hover:border-accent/40 hover:text-foreground"
              >
                full spec on github →
              </a>
              <a
                href="https://github.com/obchain/volx"
                target="_blank"
                rel="noreferrer"
                className="rounded-md px-3 py-1.5 font-medium text-soft transition-colors hover:text-foreground"
              >
                source code →
              </a>
            </div>
          </div>
        </section>

        {/* Sections */}
        <section className="mx-auto w-full max-w-4xl px-6 pb-16">
          <div className="flex flex-col gap-3">
            {SECTIONS.map((s) => (
              <article
                key={s.k}
                className="grid grid-cols-12 gap-6 rounded-2xl border border-border-subtle bg-surface px-7 py-6 transition-colors hover:border-accent/30"
              >
                <div className="col-span-12 flex items-baseline gap-3 sm:col-span-3 sm:flex-col sm:gap-2">
                  <span className="font-mono text-2xl font-semibold text-accent">{s.k}</span>
                  <h2 className="text-base font-semibold tracking-tight text-foreground">
                    {s.title}
                  </h2>
                </div>
                <div className="col-span-12 flex flex-col gap-3 text-sm leading-relaxed text-foreground sm:col-span-9">
                  {s.body}
                </div>
              </article>
            ))}
          </div>
        </section>

        {/* CTA */}
        <section className="mx-auto w-full max-w-4xl px-6 pb-20">
          <div className="flex flex-col items-center gap-4 rounded-2xl border border-border-subtle bg-surface px-8 py-10 text-center">
            <h3 className="max-w-md text-2xl font-semibold tracking-tight">
              Audit it. Run it locally.
            </h3>
            <p className="max-w-md text-sm text-muted">
              The full spec, the reference Python implementation, and the Rust engine that ships
              the published number are all open.
            </p>
            <div className="mt-2 flex flex-wrap items-center justify-center gap-3">
              <a
                href="https://github.com/obchain/volx/blob/main/METHODOLOGY.md"
                target="_blank"
                rel="noreferrer"
                className="rounded-md bg-accent px-4 py-2 text-xs font-semibold text-background transition-colors hover:bg-accent-strong"
              >
                read methodology spec
              </a>
              <Link
                href="/chart/bvol"
                className="rounded-md border border-border-subtle bg-surface px-4 py-2 text-xs font-medium text-muted transition-colors hover:border-accent/40 hover:text-foreground"
              >
                see BVOL live
              </Link>
            </div>
          </div>
        </section>
      </div>
      <Footer />
    </main>
  );
}

function Formula({ children }: { children: React.ReactNode }) {
  return (
    <pre className="overflow-x-auto rounded-lg border border-border-subtle bg-surface-2 px-4 py-3 font-mono text-xs leading-relaxed text-foreground">
      <code>{children}</code>
    </pre>
  );
}

function Factor({ k, v, note }: { k: string; v: string; note: string }) {
  return (
    <div className="flex flex-col gap-1 rounded-lg border border-border-subtle bg-surface-2 px-4 py-3">
      <div className="flex items-baseline justify-between gap-3">
        <span className="font-mono text-xs font-semibold text-accent">{k}</span>
        <span className="font-mono text-xs tabular-nums text-foreground">{v}</span>
      </div>
      <span className="text-[11px] text-soft">{note}</span>
    </div>
  );
}
