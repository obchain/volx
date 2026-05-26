"use client";

import { createContext, useContext, useEffect, useState, type ReactNode } from "react";

export type Theme = "light" | "dark";
const STORAGE_KEY = "volx:theme";

interface ThemeCtx {
  theme: Theme;
  setTheme: (next: Theme) => void;
  toggle: () => void;
}

const ThemeContext = createContext<ThemeCtx | null>(null);

function readInitialTheme(): Theme {
  if (typeof window === "undefined") return "dark";
  const stored = window.localStorage.getItem(STORAGE_KEY);
  if (stored === "light" || stored === "dark") return stored;
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

export function ThemeProvider({ children }: { children: ReactNode }) {
  // Initial state matches the value the pre-paint script set on <html>.
  const [theme, setThemeState] = useState<Theme>(() => {
    if (typeof document === "undefined") return "dark";
    const attr = document.documentElement.getAttribute("data-theme");
    return attr === "light" ? "light" : "dark";
  });

  useEffect(() => {
    document.documentElement.setAttribute("data-theme", theme);
    window.localStorage.setItem(STORAGE_KEY, theme);
    // Notify non-React consumers (the ECharts instance) that the theme
    // changed; they listen for this custom event and re-read CSS vars.
    window.dispatchEvent(new CustomEvent("volx:theme", { detail: theme }));
  }, [theme]);

  const setTheme = (next: Theme) => setThemeState(next);
  const toggle = () => setThemeState((t) => (t === "dark" ? "light" : "dark"));

  return (
    <ThemeContext.Provider value={{ theme, setTheme, toggle }}>{children}</ThemeContext.Provider>
  );
}

export function useTheme() {
  const ctx = useContext(ThemeContext);
  if (!ctx) throw new Error("useTheme must be used inside <ThemeProvider>");
  return ctx;
}

// Inline script string injected into <head> by the root layout. Runs
// before React hydrates so the page paints with the right theme and
// avoids the "flash of wrong theme" effect.
export const themeInitScript = `
(function() {
  try {
    var s = localStorage.getItem(${JSON.stringify(STORAGE_KEY)});
    var t = s === "light" || s === "dark"
      ? s
      : (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
    document.documentElement.setAttribute("data-theme", t);
  } catch (e) {
    document.documentElement.setAttribute("data-theme", "dark");
  }
})();
`;

export { readInitialTheme };
