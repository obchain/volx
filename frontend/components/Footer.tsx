import Link from "next/link";

export function Footer() {
  return (
    <footer className="mt-16 border-t border-white/5 px-6 py-6 text-xs text-foreground/40">
      <div className="mx-auto flex max-w-3xl flex-wrap items-center justify-between gap-3">
        <span>VolX — crypto volatility index</span>
        <nav className="flex gap-5">
          <Link href="/methodology" className="transition-colors hover:text-foreground/80">
            methodology
          </Link>
          <a
            href="https://github.com/obchain/volx"
            target="_blank"
            rel="noreferrer"
            className="transition-colors hover:text-foreground/80"
          >
            github
          </a>
        </nav>
      </div>
    </footer>
  );
}
