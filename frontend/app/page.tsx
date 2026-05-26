export default function HomePage() {
  return (
    <main className="min-h-screen flex flex-col items-center justify-center px-6">
      <div className="max-w-2xl text-center">
        <h1 className="text-4xl font-semibold tracking-tight">VolX</h1>
        <p className="mt-3 text-sm text-foreground/60">
          Crypto volatility index. BVOL + EVOL, 60-second cadence, multi-venue blend.
        </p>
        <p className="mt-8 text-xs text-foreground/40">scaffold — landing &amp; chart land next</p>
      </div>
    </main>
  );
}
