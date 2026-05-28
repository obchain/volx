import { notFound } from "next/navigation";
import { Chart } from "@/components/Chart";
import { Footer } from "@/components/Footer";
import { Header } from "@/components/Header";
import type { IndexId } from "@/lib/api";

type Params = Promise<{ index: string }>;

const ALLOWED: readonly IndexId[] = ["bvol", "evol"] as const;

export default async function ChartPage({ params }: { params: Params }) {
  const { index } = await params;
  if (!ALLOWED.includes(index as IndexId)) {
    notFound();
  }
  return (
    <main className="flex min-h-screen flex-col">
      <Header />
      <div className="flex-1">
        <Chart id={index as IndexId} />
      </div>
      <Footer />
    </main>
  );
}
