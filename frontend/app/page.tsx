import { Dashboard } from "@/components/Dashboard";
import { Footer } from "@/components/Footer";

export default function HomePage() {
  return (
    <main className="flex min-h-screen flex-col">
      <div className="flex flex-1 items-center justify-center py-16">
        <Dashboard />
      </div>
      <Footer />
    </main>
  );
}
