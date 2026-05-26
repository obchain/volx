import type { Metadata } from "next";
import { ThemeProvider, themeInitScript } from "@/lib/theme";
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
    <html lang="en" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: themeInitScript }} />
      </head>
      <body className="bg-background text-foreground">
        <ThemeProvider>{children}</ThemeProvider>
      </body>
    </html>
  );
}
