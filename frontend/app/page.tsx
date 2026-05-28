import { AnimatedBackground } from "@/components/AnimatedBackground";
import { Dashboard } from "@/components/Dashboard";
import { Footer } from "@/components/Footer";
import { Header } from "@/components/Header";
import { LiveStats } from "@/components/LiveStats";
import { TickerTape } from "@/components/TickerTape";
import { TrustStrip } from "@/components/TrustStrip";
import { VenueNetwork } from "@/components/VenueNetwork";

export default function HomePage() {
  return (
    <main className="relative flex min-h-screen flex-col">
      <AnimatedBackground />
      <Header />
      <TickerTape />
      <div className="flex flex-1 flex-col gap-24 pb-24">
        <Dashboard />
        <section className="mx-auto w-full max-w-5xl px-6">
          <div className="mb-8 flex items-end justify-between gap-6">
            <h2 className="max-w-md text-2xl font-semibold tracking-tight text-foreground sm:text-3xl">
              Three venues, <span className="text-accent">one number.</span>
            </h2>
            <p className="hidden max-w-md text-xs text-muted sm:block">
              Per-venue strip → median blend → 5%/5-tick outlier drop → published every 60 seconds
              with a confidence score. Hover a node to pause the animation.
            </p>
          </div>
          <div className="rounded-2xl border border-border-subtle bg-surface px-6 py-8">
            <VenueNetwork />
          </div>
        </section>
        <LiveStats />
        <TrustStrip />
      </div>
      <Footer />
    </main>
  );
}
