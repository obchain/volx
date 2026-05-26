import { notFound } from "next/navigation";
import { Chart } from "@/components/Chart";
import type { IndexId } from "@/lib/api";

type Params = Promise<{ index: string }>;

const ALLOWED: readonly IndexId[] = ["bvol", "evol"] as const;

export default async function ChartPage({ params }: { params: Params }) {
  const { index } = await params;
  if (!ALLOWED.includes(index as IndexId)) {
    notFound();
  }
  return (
    <main className="min-h-screen">
      <Chart id={index as IndexId} />
    </main>
  );
}
