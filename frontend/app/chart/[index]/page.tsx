type Params = Promise<{ index: string }>;

export default async function ChartPage({ params }: { params: Params }) {
  const { index } = await params;
  return (
    <main className="min-h-screen flex items-center justify-center px-6">
      <p className="text-sm text-foreground/60">chart — {index} — placeholder</p>
    </main>
  );
}
