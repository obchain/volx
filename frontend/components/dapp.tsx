"use client";

import { useState } from "react";
import { useWallet } from "@/lib/wallet";
import { shortAddr } from "@/lib/format";

/** Connect / account chip + Sepolia switch prompt. */
export function WalletButton() {
  const { account, isSepolia, hasProvider, connecting, connect, switchToSepolia } = useWallet();

  if (account && !isSepolia) {
    return (
      <button
        onClick={() => switchToSepolia()}
        className="rounded-lg border border-down/40 bg-down-soft px-3 py-1.5 text-xs font-semibold text-down transition-colors hover:border-down"
      >
        Wrong network — switch to Sepolia
      </button>
    );
  }
  if (account) {
    return (
      <span className="inline-flex items-center gap-2 rounded-lg border border-border bg-surface px-3 py-1.5 text-xs font-medium">
        <span className="volx-pulse h-1.5 w-1.5 rounded-full bg-up" aria-hidden />
        <span className="font-mono text-foreground">{shortAddr(account)}</span>
      </span>
    );
  }
  return (
    <button
      onClick={() => connect()}
      disabled={connecting}
      className="rounded-lg border border-accent/40 bg-accent-soft px-4 py-1.5 text-xs font-semibold text-accent transition-colors hover:border-accent disabled:opacity-60"
    >
      {connecting ? "Connecting…" : hasProvider ? "Connect wallet" : "Install MetaMask"}
    </button>
  );
}

/** Renders children only when connected to Sepolia; otherwise a prompt. */
export function NetworkGuard({ children }: { children: React.ReactNode }) {
  const { account, isSepolia } = useWallet();
  if (!account) {
    return (
      <div className="rounded-xl border border-border-subtle bg-surface p-8 text-center">
        <p className="text-sm text-muted">Connect a wallet to trade on Sepolia.</p>
        <div className="mt-4 flex justify-center">
          <WalletButton />
        </div>
      </div>
    );
  }
  if (!isSepolia) {
    return (
      <div className="rounded-xl border border-down/30 bg-down-soft p-8 text-center">
        <p className="text-sm text-foreground">This demo runs on Ethereum Sepolia.</p>
        <div className="mt-4 flex justify-center">
          <WalletButton />
        </div>
      </div>
    );
  }
  return <>{children}</>;
}

export function Card({ title, children, className = "" }: { title?: string; children: React.ReactNode; className?: string }) {
  return (
    <div className={`rounded-2xl border border-border-subtle bg-surface p-5 ${className}`}>
      {title && <h2 className="mb-4 text-xs font-semibold uppercase tracking-[0.18em] text-soft">{title}</h2>}
      {children}
    </div>
  );
}

export function Stat({ label, value, accent }: { label: string; value: string; accent?: "up" | "down" | "accent" }) {
  const color = accent === "up" ? "text-up" : accent === "down" ? "text-down" : accent === "accent" ? "text-accent" : "text-foreground";
  return (
    <div>
      <div className="text-[10px] font-medium uppercase tracking-[0.16em] text-soft">{label}</div>
      <div className={`mt-1 font-mono text-lg font-semibold tabular-nums ${color}`}>{value}</div>
    </div>
  );
}

/** Button that runs an async tx, surfacing pending state + the last error. */
export function TxButton({
  label,
  onRun,
  disabled,
  variant = "accent",
  onDone,
}: {
  label: string;
  onRun: () => Promise<void>;
  disabled?: boolean;
  variant?: "accent" | "up" | "down";
  onDone?: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const palette =
    variant === "up"
      ? "border-up/40 bg-up-soft text-up hover:border-up"
      : variant === "down"
        ? "border-down/40 bg-down-soft text-down hover:border-down"
        : "border-accent/40 bg-accent-soft text-accent hover:border-accent";

  return (
    <div className="flex flex-col gap-1">
      <button
        onClick={async () => {
          setErr(null);
          setBusy(true);
          try {
            await onRun();
            onDone?.();
          } catch (e) {
            setErr(cleanErr(e));
          } finally {
            setBusy(false);
          }
        }}
        disabled={busy || disabled}
        className={`w-full rounded-lg border px-4 py-2.5 text-sm font-semibold transition-colors disabled:opacity-50 ${palette}`}
      >
        {busy ? "Confirm in wallet…" : label}
      </button>
      {err && <p className="text-xs text-down">{err}</p>}
    </div>
  );
}

/** Trim viem's verbose revert dumps to the human-readable first line. */
export function cleanErr(e: unknown): string {
  const msg = e instanceof Error ? e.message : String(e);
  if (/User rejected|denied/i.test(msg)) return "Rejected in wallet.";
  const short = msg.split("\n")[0] ?? msg;
  return short.length > 120 ? short.slice(0, 117) + "…" : short;
}
