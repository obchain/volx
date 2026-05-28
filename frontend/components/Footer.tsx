import Link from "next/link";

// Minimal footer. Source attribution + nav. Kept narrow so it reads as
// publication-style metadata rather than a CTA.
export function Footer() {
  return (
    <footer className="mt-24 border-t border-border-subtle px-6 py-8 text-xs text-soft">
      <div className="mx-auto flex max-w-7xl flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex flex-col gap-1.5">
          <span className="font-medium text-foreground">
            Vol<span className="text-accent">X</span>
            <span className="ml-2 font-normal text-soft">— open crypto volatility index</span>
          </span>
          <span>
            data sources: <span className="font-mono">deribit</span> ·{" "}
            <span className="font-mono">okx</span> · <span className="font-mono">bybit</span> ·
            median blend · 60s cadence
          </span>
        </div>
        <nav className="flex flex-wrap gap-x-5 gap-y-2">
          <Link href="/methodology" className="transition-colors hover:text-foreground">
            methodology
          </Link>
          <Link href="/chart/bvol" className="transition-colors hover:text-foreground">
            BVOL chart
          </Link>
          <Link href="/chart/evol" className="transition-colors hover:text-foreground">
            EVOL chart
          </Link>
          <a
            href="https://github.com/obchain/volx"
            target="_blank"
            rel="noreferrer"
            className="transition-colors hover:text-foreground"
          >
            github
          </a>
          <a
            href="https://github.com/obchain/volx/blob/main/METHODOLOGY.md"
            target="_blank"
            rel="noreferrer"
            className="transition-colors hover:text-foreground"
          >
            spec
          </a>
        </nav>
      </div>
    </footer>
  );
}
