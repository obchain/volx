"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as echarts from "echarts/core";
import { CandlestickChart, BarChart, LineChart } from "echarts/charts";
import { MarkLineComponent } from "echarts/components";
import {
  GridComponent,
  TooltipComponent,
  DataZoomComponent,
  AxisPointerComponent,
} from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";
import { fetchHistory, type HistoryBar, type IndexId } from "@/lib/api";
import { useIndexTicks } from "@/lib/useIndexTicks";
import { useTheme } from "@/lib/theme";
import { DEFAULT_TIMEFRAME, TIMEFRAMES, TIMEFRAME_SPEC, type Timeframe } from "@/lib/timeframes";
import { ThemeToggle } from "./ThemeToggle";

echarts.use([
  CandlestickChart,
  BarChart,
  LineChart,
  GridComponent,
  TooltipComponent,
  DataZoomComponent,
  AxisPointerComponent,
  MarkLineComponent,
  CanvasRenderer,
]);

interface ChartProps {
  id: IndexId;
}

const NAME: Record<IndexId, string> = {
  bvol: "Bitcoin Volatility",
  evol: "Ethereum Volatility",
};

interface Stats {
  open: number;
  close: number;
  high: number;
  low: number;
  changePct: number;
  totalTicks: number;
  avgConfidence: number;
}

function computeStats(bars: HistoryBar[]): Stats | null {
  if (bars.length === 0) return null;
  const open = bars[0]!.open;
  const close = bars[bars.length - 1]!.close;
  let high = -Infinity;
  let low = Infinity;
  let totalTicks = 0;
  let confSum = 0;
  let confCount = 0;
  for (const b of bars) {
    if (b.high > high) high = b.high;
    if (b.low < low) low = b.low;
    totalTicks += b.count;
    if (b.avg_confidence > 0) {
      confSum += b.avg_confidence * b.count;
      confCount += b.count;
    }
  }
  const changePct = open === 0 ? 0 : ((close - open) / open) * 100;
  const avgConfidence = confCount === 0 ? 0 : confSum / confCount;
  return { open, close, high, low, changePct, totalTicks, avgConfidence };
}

// ECharts colors are hardcoded per theme rather than read from CSS vars.
// Reading vars introduces a race with the ThemeProvider effect that writes
// `data-theme` on <html> — if the chart redraws before the attribute lands,
// the canvas paints with the old palette. Driving the palette from the
// React-state theme value sidesteps that ordering.
type ChartPalette = {
  up: string;
  down: string;
  upGlow: string;
  downGlow: string;
  text: string;
  textStrong: string;
  textMuted: string;
  grid: string;
  gridStrong: string;
  tooltipBg: string;
  tooltipBorder: string;
  lineColor: string;
  lineGradTop: string;
  lineGradBot: string;
  ema: string;
};

function readChartTheme(theme: "dark" | "light"): ChartPalette {
  if (theme === "light") {
    return {
      up: "#16a34a",
      down: "#dc2626",
      upGlow: "rgba(22,163,74,0.35)",
      downGlow: "rgba(220,38,38,0.35)",
      text: "rgba(10,10,12,0.95)",
      textStrong: "rgba(10,10,12,1)",
      textMuted: "rgba(10,10,12,0.7)",
      grid: "rgba(10,10,12,0.06)",
      gridStrong: "rgba(10,10,12,0.12)",
      tooltipBg: "rgba(255,255,255,0.98)",
      tooltipBorder: "rgba(10,10,12,0.15)",
      lineColor: "#6366f1",
      lineGradTop: "rgba(99,102,241,0.22)",
      lineGradBot: "rgba(99,102,241,0.0)",
      ema: "rgba(234,88,12,0.85)",
    };
  }
  return {
    up: "#4ade80",
    down: "#f87171",
    upGlow: "rgba(74,222,128,0.45)",
    downGlow: "rgba(248,113,113,0.45)",
    text: "rgba(237,237,237,0.95)",
    textStrong: "rgba(255,255,255,1)",
    textMuted: "rgba(237,237,237,0.7)",
    grid: "rgba(255,255,255,0.05)",
    gridStrong: "rgba(255,255,255,0.12)",
    tooltipBg: "rgba(15,15,18,0.97)",
    tooltipBorder: "rgba(255,255,255,0.12)",
    lineColor: "#a78bfa",
    lineGradTop: "rgba(167,139,250,0.32)",
    lineGradBot: "rgba(167,139,250,0.0)",
    ema: "rgba(251,146,60,0.9)",
  };
}

// Simple EMA. Used for the trend overlay on the candle chart.
function computeEMA(values: number[], period: number): (number | "-")[] {
  if (values.length === 0) return [];
  const k = 2 / (period + 1);
  const out: (number | "-")[] = [];
  let prev: number | null = null;
  for (let i = 0; i < values.length; i++) {
    if (i < period - 1) {
      out.push("-");
      continue;
    }
    if (prev === null) {
      let sum = 0;
      for (let j = 0; j <= i; j++) sum += values[j]!;
      prev = sum / period;
      out.push(prev);
      continue;
    }
    prev = values[i]! * k + prev * (1 - k);
    out.push(prev);
  }
  return out;
}

export function Chart({ id }: ChartProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  const barsRef = useRef<HistoryBar[]>([]);
  // Held outside React state so the periodic history fetch can re-apply
  // the freshest tick after replacing `barsRef.current`.
  const latestTickRef = useRef<{ value: number; ts: number } | null>(null);
  const [timeframe, setTimeframe] = useState<Timeframe>(DEFAULT_TIMEFRAME);
  const [stats, setStats] = useState<Stats | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const { theme } = useTheme();

  const { state: wsState, ticks } = useIndexTicks([id]);
  const liveTick = ticks[id];

  useEffect(() => {
    if (!containerRef.current) return;
    const chart = echarts.init(containerRef.current, undefined, { renderer: "canvas" });
    chartRef.current = chart;
    const ro = new ResizeObserver(() => chart.resize());
    ro.observe(containerRef.current);
    return () => {
      ro.disconnect();
      chart.dispose();
      chartRef.current = null;
    };
  }, []);

  const draw = useCallback(
    (bars: HistoryBar[]) => {
      if (!chartRef.current) return;
      const t = readChartTheme(theme);
      const ohlc = bars.map((b) => [b.open, b.close, b.low, b.high]);
      const closes = bars.map((b) => b.close);
      const ema = computeEMA(closes, Math.min(20, Math.max(4, Math.floor(bars.length / 6))));
      const volumes = bars.map((b) => ({
        value: b.count,
        itemStyle: {
          color: b.close >= b.open ? t.up : t.down,
          opacity: 0.65,
          borderRadius: [2, 2, 0, 0],
        },
      }));
      const labels = bars.map((b) => b.ts);
      const lastClose = closes[closes.length - 1];

      chartRef.current.setOption(
        {
          animation: true,
          animationDuration: 250,
          backgroundColor: "transparent",
          textStyle: {
            color: t.text,
            fontFamily: "ui-sans-serif, system-ui",
            fontSize: 12,
          },
          grid: [
            { left: 64, right: 22, top: 14, height: "64%" },
            { left: 64, right: 22, top: "76%", height: "18%" },
          ],
          xAxis: [
            {
              type: "category",
              data: labels,
              axisLine: { lineStyle: { color: t.gridStrong } },
              axisLabel: {
                color: t.text,
                fontSize: 11,
                fontWeight: 500,
                margin: 12,
                hideOverlap: true,
                formatter: (v: string) => {
                  const d = new Date(v);
                  if (isNaN(d.getTime())) return v;
                  const h = String(d.getHours()).padStart(2, "0");
                  const m = String(d.getMinutes()).padStart(2, "0");
                  const day = String(d.getDate()).padStart(2, "0");
                  const mon = d.toLocaleString("en", { month: "short" });
                  return `${day} ${mon}  ${h}:${m}`;
                },
              },
              splitLine: { show: false },
              axisTick: { show: false },
              boundaryGap: true,
            },
            {
              type: "category",
              gridIndex: 1,
              data: labels,
              axisLine: { lineStyle: { color: t.gridStrong } },
              axisLabel: { show: false },
              axisTick: { show: false },
              splitLine: { show: false },
            },
          ],
          yAxis: [
            {
              scale: true,
              position: "right",
              axisLine: { show: false },
              splitLine: { lineStyle: { color: t.grid, type: "dashed" } },
              axisLabel: {
                color: t.text,
                fontSize: 12,
                fontWeight: 600,
                margin: 10,
                formatter: (v: number) => v.toFixed(2),
              },
            },
            {
              scale: true,
              gridIndex: 1,
              position: "right",
              axisLine: { show: false },
              splitLine: { show: false },
              axisLabel: {
                color: t.textMuted,
                fontSize: 11,
                fontWeight: 500,
                margin: 10,
                formatter: (v: number) => (v >= 1000 ? `${(v / 1000).toFixed(1)}k` : v.toFixed(0)),
              },
              max: (value: { max: number }) => Math.ceil(value.max * 1.1),
            },
          ],
          axisPointer: {
            link: [{ xAxisIndex: "all" }],
            lineStyle: { color: t.gridStrong, width: 1, type: "solid" },
            label: {
              backgroundColor: t.tooltipBg,
              borderColor: t.tooltipBorder,
              borderWidth: 1,
              color: t.textStrong,
              fontSize: 11,
              fontWeight: 600,
              padding: [4, 8],
              shadowBlur: 8,
              shadowColor: "rgba(0,0,0,0.25)",
            },
          },
          tooltip: {
            trigger: "axis",
            backgroundColor: t.tooltipBg,
            borderColor: t.tooltipBorder,
            borderWidth: 1,
            padding: [10, 12],
            extraCssText: `border-radius:10px;backdrop-filter:blur(10px);box-shadow:0 8px 24px rgba(0,0,0,0.35)`,
            textStyle: { color: t.text, fontSize: 12 },
            axisPointer: { type: "cross" },
            formatter: (params: unknown) => {
              const arr = params as Array<{
                seriesType: string;
                value: number[] | number;
                axisValue: string;
              }>;
              const candle = arr.find((p) => p.seriesType === "candlestick");
              if (!candle || !Array.isArray(candle.value)) return "";
              const [, open, close, low, high] = candle.value;
              const change = ((close - open) / open) * 100;
              const range = high - low;
              const sign = change >= 0 ? "+" : "";
              return `
                <div style="font-size:10px;color:${t.textMuted};letter-spacing:0.05em;text-transform:uppercase;margin-bottom:8px">${candle.axisValue}</div>
                <div style="display:grid;grid-template-columns:auto auto;gap:4px 16px;font-size:12px;font-weight:500">
                  <span style="color:${t.textMuted}">open</span><span style="color:${t.textStrong};font-variant-numeric:tabular-nums">${open.toFixed(2)}</span>
                  <span style="color:${t.textMuted}">high</span><span style="color:${t.textStrong};font-variant-numeric:tabular-nums">${high.toFixed(2)}</span>
                  <span style="color:${t.textMuted}">low</span><span style="color:${t.textStrong};font-variant-numeric:tabular-nums">${low.toFixed(2)}</span>
                  <span style="color:${t.textMuted}">close</span><span style="color:${t.textStrong};font-variant-numeric:tabular-nums">${close.toFixed(2)}</span>
                  <span style="color:${t.textMuted}">range</span><span style="font-variant-numeric:tabular-nums">${range.toFixed(2)}</span>
                  <span style="color:${t.textMuted}">change</span><span style="color:${change >= 0 ? t.up : t.down};font-weight:600;font-variant-numeric:tabular-nums">${sign}${change.toFixed(2)}%</span>
                </div>`;
            },
          },
          dataZoom: [{ type: "inside", xAxisIndex: [0, 1], start: 0, end: 100 }],
          series: [
            // Area-gradient under closes — gives the chart a "filled" look without
            // hiding the candles. Drawn first so candles render on top.
            {
              type: "line",
              name: "close",
              data: closes,
              smooth: 0.3,
              symbol: "none",
              lineStyle: { color: t.lineColor, width: 1.4, opacity: 0.55 },
              areaStyle: {
                color: new echarts.graphic.LinearGradient(0, 0, 0, 1, [
                  { offset: 0, color: t.lineGradTop },
                  { offset: 1, color: t.lineGradBot },
                ]),
              },
              z: 1,
              tooltip: { show: false },
            },
            // EMA trend overlay.
            {
              type: "line",
              name: "ema",
              data: ema,
              smooth: true,
              symbol: "none",
              lineStyle: {
                color: t.ema,
                width: 1.5,
                opacity: 0.85,
                type: "solid",
              },
              z: 2,
              tooltip: { show: false },
            },
            // Candles. Glow via shadow.
            {
              type: "candlestick",
              name: id.toUpperCase(),
              data: ohlc,
              itemStyle: {
                color: t.up,
                color0: t.down,
                borderColor: t.up,
                borderColor0: t.down,
                borderWidth: 1,
                shadowBlur: 6,
                shadowColor: t.upGlow,
              },
              barMaxWidth: 14,
              z: 3,
              markLine:
                lastClose !== undefined
                  ? {
                      symbol: "none",
                      silent: true,
                      lineStyle: {
                        color: t.lineColor,
                        type: "dashed",
                        width: 1,
                        opacity: 0.7,
                      },
                      label: {
                        color: t.textStrong,
                        backgroundColor: t.tooltipBg,
                        borderColor: t.tooltipBorder,
                        borderWidth: 1,
                        padding: [3, 6],
                        borderRadius: 4,
                        fontSize: 11,
                        fontWeight: 600,
                        formatter: lastClose.toFixed(2),
                        position: "insideEndTop",
                      },
                      data: [{ yAxis: lastClose }],
                    }
                  : undefined,
            },
            {
              type: "bar",
              name: "ticks",
              xAxisIndex: 1,
              yAxisIndex: 1,
              data: volumes,
              barMaxWidth: 14,
            },
          ],
        },
        true,
      );
    },
    [id, theme],
  );

  // `draw` is captured in a ref so the long-lived interval effect below can
  // read the latest function without taking `draw` as a dependency. Without
  // this, a `theme` flip recreates `draw`, which would cancel the interval
  // and trigger a redundant history refetch on every toggle.
  const drawRef = useRef(draw);
  useEffect(() => {
    drawRef.current = draw;
  }, [draw]);

  // Re-applies the latest WS tick to the last bar in `barsRef`. Used after
  // a history refetch overwrites the bar set — if the tick is fresher than
  // the server-side last bar, patch it back so the painted candle keeps
  // up.
  const applyLatestTickToLastBar = () => {
    const tick = latestTickRef.current;
    if (!tick || barsRef.current.length === 0) return;
    const last = barsRef.current[barsRef.current.length - 1]!;
    const lastBarMs = Date.parse(last.ts);
    if (Number.isNaN(lastBarMs) || tick.ts < lastBarMs) return;
    barsRef.current[barsRef.current.length - 1] = {
      ...last,
      high: Math.max(last.high, tick.value),
      low: Math.min(last.low, tick.value),
      close: tick.value,
    };
  };

  useEffect(() => {
    let cancelled = false;
    const spec = TIMEFRAME_SPEC[timeframe];
    setLoadErr(null);
    const run = async () => {
      try {
        const hist = await fetchHistory(id, spec.interval, spec.limit);
        if (cancelled) return;
        barsRef.current = hist.bars;
        applyLatestTickToLastBar();
        setStats(computeStats(barsRef.current));
        drawRef.current(barsRef.current);
      } catch (e) {
        if (!cancelled) setLoadErr(e instanceof Error ? e.message : String(e));
      }
    };
    run();
    const handle = window.setInterval(run, 60_000);
    return () => {
      cancelled = true;
      window.clearInterval(handle);
    };
  }, [id, timeframe]);

  // Redraw when theme flips so ECharts picks up the new palette.
  useEffect(() => {
    if (barsRef.current.length > 0) drawRef.current(barsRef.current);
  }, [theme]);

  // Live tick → patch last candle in place + redraw via `drawRef` so every
  // close-dependent series (line area, EMA overlay, mark-line) stays in
  // sync with the candle. Targeting one series by index would silently
  // overwrite the wrong layer because partial-option series merge by
  // position, not by id, unless every series carries an explicit id.
  useEffect(() => {
    if (!liveTick || !chartRef.current || barsRef.current.length === 0) return;
    latestTickRef.current = { value: liveTick.value, ts: liveTick.ts };
    const last = barsRef.current[barsRef.current.length - 1]!;
    barsRef.current[barsRef.current.length - 1] = {
      ...last,
      high: Math.max(last.high, liveTick.value),
      low: Math.min(last.low, liveTick.value),
      close: liveTick.value,
    };
    setStats(computeStats(barsRef.current));
    drawRef.current(barsRef.current);
  }, [liveTick]);

  const value = liveTick?.value ?? stats?.close;
  const live = wsState === "open";

  const headerDelta = useMemo<{ label: string; tone: "up" | "down" } | null>(() => {
    if (!stats) return null;
    const sign = stats.changePct >= 0 ? "+" : "";
    return {
      label: `${sign}${stats.changePct.toFixed(2)}%`,
      tone: stats.changePct >= 0 ? "up" : "down",
    };
  }, [stats]);

  return (
    <div className="mx-auto w-full max-w-screen-2xl px-4 py-5 sm:px-6 lg:px-10">
      <section className="flex flex-col gap-2">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2 text-[10px] uppercase tracking-[0.25em] text-soft">
            <span>VolX</span>
            <span className="text-soft-2">/</span>
            <span className="text-muted">{id}</span>
            <span className="text-soft-2">— {NAME[id]}</span>
          </div>
          <ThemeToggle />
        </div>
        <div className="flex flex-wrap items-end justify-between gap-4">
          <div className="flex items-end gap-4">
            <span className="text-5xl font-semibold tabular-nums tracking-tight sm:text-6xl">
              {value !== undefined ? value.toFixed(2) : "—"}
            </span>
            {headerDelta && (
              <span
                className={
                  headerDelta.tone === "up"
                    ? "mb-1 inline-flex rounded-full bg-up-soft px-2.5 py-0.5 text-sm font-medium text-up"
                    : "mb-1 inline-flex rounded-full bg-down-soft px-2.5 py-0.5 text-sm font-medium text-down"
                }
              >
                {headerDelta.label} {timeframe}
              </span>
            )}
          </div>
          <div className="flex items-center gap-3 text-[10px] uppercase tracking-widest text-soft">
            <span className="inline-flex items-center gap-1.5">
              <span
                className={`inline-block h-1.5 w-1.5 rounded-full ${
                  live ? "bg-up" : "bg-amber-400"
                }`}
              />
              {live ? "live" : wsState}
            </span>
            <span className="text-soft-2">•</span>
            <span>60 s cadence</span>
          </div>
        </div>
      </section>

      <section className="mt-5 grid grid-cols-2 gap-3 rounded-xl border border-border-subtle bg-surface p-4 sm:grid-cols-3 md:grid-cols-6">
        <Stat label="open" value={stats?.open?.toFixed(2)} />
        <Stat label="close" value={stats?.close?.toFixed(2)} />
        <Stat label="high" value={stats?.high?.toFixed(2)} />
        <Stat label="low" value={stats?.low?.toFixed(2)} />
        <Stat label="ticks" value={stats?.totalTicks?.toString()} />
        <Stat label="avg conf" value={stats?.avgConfidence?.toFixed(2)} />
      </section>

      <section className="mt-5 grid grid-cols-12 gap-4">
        <div className="col-span-12 xl:col-span-9">
          <div className="mb-3 flex items-center justify-between gap-2">
            <nav className="inline-flex overflow-x-auto rounded-lg border border-border-subtle bg-surface p-0.5 text-xs">
              {TIMEFRAMES.map((tf) => (
                <button
                  key={tf}
                  type="button"
                  onClick={() => setTimeframe(tf)}
                  className={
                    tf === timeframe
                      ? "rounded-md bg-surface-2 px-3 py-1 text-foreground"
                      : "rounded-md px-3 py-1 text-soft transition-colors hover:text-foreground"
                  }
                >
                  {tf}
                </button>
              ))}
            </nav>
            <span className="text-[10px] uppercase tracking-widest text-soft-2">
              {TIMEFRAME_SPEC[timeframe].interval} bars
            </span>
          </div>
          <div className="relative overflow-hidden rounded-2xl border border-border-subtle bg-surface">
            <div
              ref={containerRef}
              className="aspect-[16/9] w-full min-h-[420px] xl:aspect-[21/9] xl:min-h-[560px]"
            />
            {loadErr && (
              <p className="absolute inset-0 flex items-center justify-center text-xs text-down/80">
                history fetch failed: {loadErr}
              </p>
            )}
          </div>
        </div>

        <aside className="col-span-12 flex flex-col gap-3 xl:col-span-3">
          <RailCard title="About this index">
            <p className="text-xs leading-relaxed text-muted">
              {id === "bvol"
                ? "30-day implied volatility for BTC, computed per venue from a strip of OTM options on Deribit, OKX, and Bybit, then median-blended across venues via the CBOE-style variance integral. Updated every 60 seconds."
                : "30-day implied volatility for ETH, computed per venue from a strip of OTM options on Deribit, OKX, and Bybit, then median-blended across venues via the CBOE-style variance integral. Updated every 60 seconds."}
            </p>
          </RailCard>

          <RailCard title="Live snapshot">
            <Row k="last value" v={value !== undefined ? value.toFixed(2) : "—"} />
            <Row
              k="confidence"
              v={
                liveTick?.confidence !== undefined
                  ? liveTick.confidence.toFixed(2)
                  : stats?.avgConfidence !== undefined
                    ? stats.avgConfidence.toFixed(2)
                    : "—"
              }
            />
            <Row k="last tick" v={liveTick ? new Date(liveTick.ts).toLocaleTimeString() : "—"} />
            <Row k="cadence" v="60 s" />
          </RailCard>

          <RailCard title="Window">
            <Row k="bars" v={String(barsRef.current.length)} />
            <Row k="interval" v={TIMEFRAME_SPEC[timeframe].interval} />
            <Row k="change" v={headerDelta ? headerDelta.label : "—"} tone={headerDelta?.tone} />
          </RailCard>
        </aside>
      </section>

      <footer className="mt-6 flex flex-wrap items-center justify-between gap-2 text-[11px] text-soft">
        <span>data sources: deribit · okx · bybit · median blend · CBOE-style variance integral</span>
        <span>
          {liveTick
            ? `updated ${new Date(liveTick.ts).toLocaleTimeString()}`
            : stats
              ? "history bootstrap"
              : "loading…"}
        </span>
      </footer>
    </div>
  );
}

function Stat({ label, value }: { label: string; value?: string }) {
  return (
    <div className="flex flex-col gap-0.5">
      <span className="text-[10px] uppercase tracking-widest text-soft-2">{label}</span>
      <span className="text-base font-medium tabular-nums sm:text-lg">{value ?? "—"}</span>
    </div>
  );
}

function RailCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="rounded-xl border border-border-subtle bg-surface p-4">
      <h3 className="mb-3 text-[10px] uppercase tracking-widest text-soft">{title}</h3>
      <div className="flex flex-col gap-1.5">{children}</div>
    </div>
  );
}

function Row({ k, v, tone }: { k: string; v: string; tone?: "up" | "down" }) {
  const valueCls = tone === "up" ? "text-up" : tone === "down" ? "text-down" : "text-foreground";
  return (
    <div className="flex items-baseline justify-between text-xs">
      <span className="text-soft">{k}</span>
      <span className={`tabular-nums ${valueCls}`}>{v}</span>
    </div>
  );
}
