"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as echarts from "echarts/core";
import { CandlestickChart, LineChart } from "echarts/charts";
import { GridComponent, TooltipComponent, AxisPointerComponent, MarkLineComponent } from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";
import { fetchHistory, type HistoryBar, type IndexId } from "@/lib/api";
import { useIndexTicks } from "@/lib/useIndexTicks";
import { useTheme } from "@/lib/theme";
import { readChartTheme } from "./Chart";
import { DEFAULT_TIMEFRAME, TIMEFRAMES, TIMEFRAME_SPEC, type Timeframe } from "@/lib/timeframes";
import { LivePulse } from "./LivePulse";
import { liqPriceVol } from "@/lib/perp";

/** An open position to overlay (entry + liquidation lines) on the chart. */
export interface ChartPosition {
  id: string;
  isLong: boolean;
  leverage: number;
  entry: number; // index vol points
}

/** The order being configured — draws a hypothetical liquidation line at the
 * current mark so the trader sees the risk before opening. */
export interface ChartPreview {
  isLong: boolean;
  leverage: number;
}

echarts.use([
  CandlestickChart,
  LineChart,
  GridComponent,
  TooltipComponent,
  AxisPointerComponent,
  MarkLineComponent,
  CanvasRenderer,
]);

const NAME: Record<IndexId, string> = { bvol: "Bitcoin Volatility", evol: "Ethereum Volatility" };

/** Compact price chart for the trade ticket — shares the index data plumbing
 * (history + live WS) with the full /chart view but renders a lean
 * candles-only panel that sits beside the order form. */
export function TradeChart({
  id,
  positions = [],
  preview = null,
}: {
  id: IndexId;
  positions?: ChartPosition[];
  preview?: ChartPreview | null;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  const barsRef = useRef<HistoryBar[]>([]);
  const [timeframe, setTimeframe] = useState<Timeframe>(DEFAULT_TIMEFRAME);
  const [err, setErr] = useState<string | null>(null);
  const [openClose, setOpenClose] = useState<{ open: number; close: number } | null>(null);
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
      const labels = bars.map((b) => b.ts);
      const last = closes[closes.length - 1];

      // Build the overlay lines: current mark, each open position's entry +
      // liquidation, and a hypothetical liq line for the order being sized.
      type MarkItem = Record<string, unknown>;
      const lbl = (text: string, color: string, bg: string, border: string, pos: string): MarkItem => ({
        formatter: text,
        color,
        backgroundColor: bg,
        borderColor: border,
        borderWidth: 1,
        padding: [2, 5],
        borderRadius: 4,
        fontSize: 10,
        fontWeight: 700,
        position: pos,
      });
      const markLineData: MarkItem[] = [];
      if (last !== undefined) {
        markLineData.push({
          yAxis: last,
          lineStyle: { color: t.lineColor, type: "dashed", width: 1, opacity: 0.7 },
          label: lbl(last.toFixed(2), t.textStrong, t.tooltipBg, t.tooltipBorder, "insideEndTop"),
        });
      }
      for (const p of positions) {
        const c = p.isLong ? t.up : t.down;
        markLineData.push({
          yAxis: p.entry,
          lineStyle: { color: c, type: "solid", width: 1.5, opacity: 0.9 },
          label: lbl(`${p.isLong ? "L" : "S"}${p.leverage}× entry`, "#fff", c, c, "insideStartTop"),
        });
        markLineData.push({
          yAxis: liqPriceVol(p.entry, p.leverage, p.isLong),
          lineStyle: { color: t.down, type: "dashed", width: 1.2, opacity: 0.85 },
          label: lbl("liq", t.down, t.tooltipBg, t.down, "insideStartBottom"),
        });
      }
      if (preview && last !== undefined) {
        markLineData.push({
          yAxis: liqPriceVol(last, preview.leverage, preview.isLong),
          lineStyle: { color: t.ema, type: "dashed", width: 1.2, opacity: 0.75 },
          label: lbl(`liq @${preview.leverage}×`, t.ema, t.tooltipBg, t.tooltipBorder, "insideEndBottom"),
        });
      }

      chartRef.current.setOption(
        {
          animation: true,
          animationDuration: 200,
          backgroundColor: "transparent",
          textStyle: { color: t.text, fontFamily: "ui-sans-serif, system-ui", fontSize: 11 },
          grid: { left: 8, right: 56, top: 12, bottom: 22 },
          xAxis: {
            type: "category",
            data: labels,
            boundaryGap: true,
            axisLine: { lineStyle: { color: t.gridStrong } },
            axisTick: { show: false },
            splitLine: { show: false },
            axisLabel: {
              color: t.textMuted,
              fontSize: 10,
              hideOverlap: true,
              formatter: (v: string) => {
                const d = new Date(v);
                if (isNaN(d.getTime())) return v;
                return `${String(d.getDate()).padStart(2, "0")} ${d.toLocaleString("en", { month: "short" })} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
              },
            },
          },
          yAxis: {
            scale: true,
            position: "right",
            axisLine: { show: false },
            splitLine: { lineStyle: { color: t.grid, type: "dashed" } },
            axisLabel: { color: t.textMuted, fontSize: 11, fontWeight: 600, formatter: (v: number) => v.toFixed(1) },
          },
          axisPointer: { lineStyle: { color: t.gridStrong, width: 1 } },
          tooltip: {
            trigger: "axis",
            backgroundColor: t.tooltipBg,
            borderColor: t.tooltipBorder,
            borderWidth: 1,
            padding: [8, 10],
            extraCssText: "border-radius:8px;backdrop-filter:blur(8px)",
            textStyle: { color: t.text, fontSize: 11 },
            axisPointer: { type: "cross" },
            formatter: (params: unknown) => {
              const arr = params as Array<{ seriesType: string; value: number[] | number; axisValue: string }>;
              const c = arr.find((p) => p.seriesType === "candlestick");
              if (!c || !Array.isArray(c.value)) return "";
              const [, open, close, low, high] = c.value;
              const ch = open === 0 ? 0 : ((close - open) / open) * 100;
              return `<div style="font-size:10px;color:${t.textMuted};margin-bottom:4px">${c.axisValue}</div>
                <div style="font-variant-numeric:tabular-nums">O ${open.toFixed(2)} · H ${high.toFixed(2)} · L ${low.toFixed(2)} · C ${close.toFixed(2)}
                <span style="color:${ch >= 0 ? t.up : t.down}"> (${ch >= 0 ? "+" : ""}${ch.toFixed(2)}%)</span></div>`;
            },
          },
          series: [
            {
              type: "line",
              data: closes,
              smooth: 0.3,
              symbol: "none",
              lineStyle: { color: t.lineColor, width: 1.3, opacity: 0.5 },
              areaStyle: {
                color: new echarts.graphic.LinearGradient(0, 0, 0, 1, [
                  { offset: 0, color: t.lineGradTop },
                  { offset: 1, color: t.lineGradBot },
                ]),
              },
              z: 1,
              tooltip: { show: false },
            },
            {
              type: "candlestick",
              name: id.toUpperCase(),
              data: ohlc,
              itemStyle: { color: t.up, color0: t.down, borderColor: t.up, borderColor0: t.down, borderWidth: 1 },
              barMaxWidth: 10,
              z: 2,
              markLine: markLineData.length
                ? { symbol: "none", silent: true, data: markLineData }
                : undefined,
            },
          ],
        },
        true,
      );
    },
    [id, theme, positions, preview],
  );

  const drawRef = useRef(draw);
  useEffect(() => {
    drawRef.current = draw;
  }, [draw]);

  // History fetch on id/timeframe change + 60s refresh.
  useEffect(() => {
    let cancelled = false;
    // Clear stale bars on id/timeframe switch so an in-flight live tick can't
    // patch the previous index's last candle before fresh history lands.
    barsRef.current = [];
    setOpenClose(null);
    const spec = TIMEFRAME_SPEC[timeframe];
    setErr(null);
    const run = async () => {
      try {
        const hist = await fetchHistory(id, spec.interval, spec.limit);
        if (cancelled) return;
        barsRef.current = hist.bars;
        const bs = hist.bars;
        setOpenClose(bs.length ? { open: bs[0]!.open, close: bs[bs.length - 1]!.close } : null);
        drawRef.current(bs);
      } catch (e) {
        if (!cancelled) setErr(e instanceof Error ? e.message : String(e));
      }
    };
    run();
    const h = window.setInterval(run, 60_000);
    return () => {
      cancelled = true;
      window.clearInterval(h);
    };
  }, [id, timeframe]);

  // Redraw on theme flip or when the position / preview overlays change.
  useEffect(() => {
    if (barsRef.current.length > 0) drawRef.current(barsRef.current);
  }, [theme, positions, preview]);

  // Live tick → patch last candle.
  useEffect(() => {
    if (!liveTick || !chartRef.current || barsRef.current.length === 0) return;
    const last = barsRef.current[barsRef.current.length - 1]!;
    barsRef.current[barsRef.current.length - 1] = {
      ...last,
      high: Math.max(last.high, liveTick.value),
      low: Math.min(last.low, liveTick.value),
      close: liveTick.value,
    };
    setOpenClose((p) => (p ? { ...p, close: liveTick.value } : p));
    drawRef.current(barsRef.current);
  }, [liveTick]);

  const price = liveTick?.value ?? openClose?.close;
  const changePct = useMemo(() => {
    if (!openClose || openClose.open === 0) return null;
    return ((openClose.close - openClose.open) / openClose.open) * 100;
  }, [openClose]);

  return (
    <div className="rounded-2xl border border-border-subtle bg-surface p-4">
      <div className="flex items-center justify-between gap-3">
        <div className="flex items-baseline gap-3">
          <span className="text-[10px] font-semibold uppercase tracking-[0.18em] text-accent">{id.toUpperCase()}</span>
          <span className="hidden text-[11px] text-soft sm:inline">{NAME[id]}</span>
          <span className="font-mono text-2xl font-semibold tabular-nums">{price !== undefined ? price.toFixed(2) : "—"}</span>
          {changePct !== null && (
            <span className={`font-mono text-xs font-semibold tabular-nums ${changePct >= 0 ? "text-up" : "text-down"}`}>
              {changePct >= 0 ? "+" : ""}
              {changePct.toFixed(2)}%
            </span>
          )}
        </div>
        <div className="flex items-center gap-3">
          <LivePulse state={wsState} />
          <nav className="inline-flex rounded-lg border border-border-subtle bg-background/40 p-0.5 text-[11px]">
            {TIMEFRAMES.map((tf) => (
              <button
                key={tf}
                onClick={() => setTimeframe(tf)}
                className={tf === timeframe ? "rounded-md bg-surface-2 px-2 py-1 text-foreground" : "rounded-md px-2 py-1 text-soft hover:text-foreground"}
              >
                {tf}
              </button>
            ))}
          </nav>
        </div>
      </div>
      <div className="relative mt-3">
        <div ref={containerRef} className="h-[340px] w-full" />
        {err && <p className="absolute inset-0 flex items-center justify-center text-xs text-down/80">chart data failed: {err}</p>}
      </div>
    </div>
  );
}
