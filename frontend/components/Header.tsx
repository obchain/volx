"use client";

import Link from "next/link";
import type { Route } from "next";
import { usePathname } from "next/navigation";
import { useIndexTicks, type ConnState } from "@/lib/useIndexTicks";
import { ThemeToggle } from "./ThemeToggle";
import { LivePulse } from "./LivePulse";
import type { IndexId } from "@/lib/api";

const CHANNELS: IndexId[] = ["bvol", "evol"];

// Sticky top chrome. Wordmark on the left, two live BVOL/EVOL pills
// in the middle, methodology + github + theme toggle on the right.
// The pills double as deep-links to the chart pages and update in real
// time from the same WebSocket the rest of the app uses.
export function Header() {
  const pathname = usePathname();
  const { state, ticks } = useIndexTicks(CHANNELS);

  return (
    <header className="sticky top-0 z-40 w-full border-b border-border-subtle bg-background/80 backdrop-blur-md">
      <div className="mx-auto flex h-14 max-w-7xl items-center justify-between gap-4 px-5">
        {/* Left: wordmark + live state */}
        <div className="flex items-center gap-5">
          <Link href="/" className="group flex items-center gap-2.5">
            <Wordmark />
            <span className="hidden text-[10px] font-medium uppercase tracking-[0.22em] text-soft sm:inline">
              crypto vol index
            </span>
          </Link>
          <span className="hidden h-4 w-px bg-border sm:inline-block" />
          <span className="hidden sm:inline-flex">
            <LivePulse state={state} />
          </span>
        </div>

        {/* Center: live BVOL + EVOL pills */}
        <div className="flex items-center gap-2">
          <IndexPill id="bvol" label="BVOL" value={ticks.bvol?.value} state={state} />
          <IndexPill id="evol" label="EVOL" value={ticks.evol?.value} state={state} />
        </div>

        {/* Right: nav + theme */}
        <nav className="flex items-center gap-1">
          <NavLink href="/methodology" active={pathname === "/methodology"}>
            methodology
          </NavLink>
          <a
            href="https://github.com/obchain/volx"
            target="_blank"
            rel="noreferrer"
            className="rounded-md px-3 py-1.5 text-xs font-medium text-muted transition-colors hover:bg-surface hover:text-foreground"
          >
            github
          </a>
          <span className="ml-1">
            <ThemeToggle />
          </span>
        </nav>
      </div>
    </header>
  );
}

function Wordmark() {
  return (
    <span className="text-base font-semibold tracking-tight text-foreground">
      Vol<span className="text-accent">X</span>
    </span>
  );
}

function NavLink({
  href,
  active,
  children,
}: {
  // next/link with `experimental.typedRoutes` requires the href to be
  // the typed `Route` union, not a plain string. Callers pass route
  // literals (e.g. "/methodology") so the type narrows at the call site.
  href: Route;
  active: boolean;
  children: React.ReactNode;
}) {
  return (
    <Link
      href={href}
      className={`rounded-md px-3 py-1.5 text-xs font-medium transition-colors ${
        active
          ? "bg-surface text-foreground"
          : "text-muted hover:bg-surface hover:text-foreground"
      }`}
    >
      {children}
    </Link>
  );
}

function IndexPill({
  id,
  label,
  value,
  state,
}: {
  id: IndexId;
  label: string;
  value: number | undefined;
  state: ConnState;
}) {
  const display = value !== undefined ? value.toFixed(2) : "—";
  const isLive = state === "open" && value !== undefined;

  return (
    <Link
      href={`/chart/${id}`}
      className="group flex items-center gap-2 rounded-md border border-border-subtle bg-surface px-2.5 py-1.5 transition-all hover:border-accent/40 hover:bg-accent-soft"
    >
      <span className="text-[10px] font-semibold uppercase tracking-[0.18em] text-muted group-hover:text-foreground">
        {label}
      </span>
      <span className="font-mono text-sm font-semibold tabular-nums text-foreground">
        {display}
      </span>
      {isLive && (
        <span
          aria-hidden
          className="volx-pulse h-1 w-1 rounded-full bg-accent"
        />
      )}
    </Link>
  );
}
