import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "VolX",
  description: "Crypto volatility index — BVOL + EVOL, 60s cadence, multi-venue.",
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
