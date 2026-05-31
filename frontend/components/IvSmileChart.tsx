"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import * as echarts from "echarts/core";
import { LineChart, ScatterChart } from "echarts/charts";
import { GridComponent, TooltipComponent, MarkLineComponent } from "echarts/components";
import { CanvasRenderer } from "echarts/renderers";
import { fetchStrip, type IndexId, type StripExpiry } from "@/lib/api";
import { useTheme } from "@/lib/theme";
import { readChartTheme } from "./Chart";

echarts.use([LineChart, ScatterChart, GridComponent, TooltipComponent, MarkLineComponent, CanvasRenderer]);

/** Implied-volatility smile for one index's front-expiry strip — strike (x) vs
 * IV (y). This is the raw input the variance integral runs over; most index
 * dashboards never expose it. */
export function IvSmileChart({ id }: { id: IndexId }) {
  const ref = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  const [strip, setStrip] = useState<StripExpiry | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const { theme } = useTheme();

  useEffect(() => {
    if (!ref.current) return;
    const c = echarts.init(ref.current, undefined, { renderer: "canvas" });
    chartRef.current = c;
    const ro = new ResizeObserver(() => c.resize());
    ro.observe(ref.current);
    return () => {
      ro.disconnect();
      c.dispose();
      chartRef.current = null;
    };
  }, []);

  useEffect(() => {
    let alive = true;
    setErr(null);
    const load = () =>
      fetchStrip(id)
        .then((s) => {
          if (alive) setStrip(s.near ?? null);
        })
        .catch((e) => alive && setErr(e instanceof Error ? e.message : String(e)));
    load();
    const t = setInterval(load, 60_000);
    return () => {
      alive = false;
      clearInterval(t);
    };
  }, [id]);

  const draw = useCallback(() => {
    if (!chartRef.current || !strip) return;
    const t = readChartTheme(theme);
    // [strike, q_usd, iv] -> [strike, iv%]
    const pts = strip.quotes.map((q) => [q[0], q[2] * 100]);
    chartRef.current.setOption(
      {
        backgroundColor: "transparent",
        textStyle: { color: t.text, fontFamily: "ui-sans-serif, system-ui", fontSize: 11 },
        grid: { left: 48, right: 18, top: 16, bottom: 30 },
        xAxis: {
          type: "value",
          scale: true,
          name: "strike (USD)",
          nameLocation: "middle",
          nameGap: 26,
          nameTextStyle: { color: t.textMuted, fontSize: 10 },
          axisLine: { lineStyle: { color: t.gridStrong } },
          splitLine: { lineStyle: { color: t.grid, type: "dashed" } },
          axisLabel: { color: t.textMuted, fontSize: 10, formatter: (v: number) => (v >= 1000 ? `${(v / 1000).toFixed(0)}k` : v.toFixed(0)) },
        },
        yAxis: {
          type: "value",
          scale: true,
          name: "IV %",
          nameTextStyle: { color: t.textMuted, fontSize: 10 },
          axisLine: { show: false },
          splitLine: { lineStyle: { color: t.grid, type: "dashed" } },
          axisLabel: { color: t.textMuted, fontSize: 11, fontWeight: 600, formatter: (v: number) => v.toFixed(0) },
        },
        tooltip: {
          trigger: "axis",
          backgroundColor: t.tooltipBg,
          borderColor: t.tooltipBorder,
          borderWidth: 1,
          textStyle: { color: t.text, fontSize: 11 },
          formatter: (p: unknown) => {
            const a = (p as { value: number[] }[])[0];
            if (!a) return "";
            return `strike ${a.value[0]!.toLocaleString()}<br/>IV <b>${a.value[1]!.toFixed(1)}%</b>`;
          },
        },
        series: [
          {
            type: "line",
            data: pts,
            smooth: 0.4,
            symbol: "circle",
            symbolSize: 4,
            itemStyle: { color: t.lineColor },
            lineStyle: { color: t.lineColor, width: 1.6 },
            areaStyle: {
              color: new echarts.graphic.LinearGradient(0, 0, 0, 1, [
                { offset: 0, color: t.lineGradTop },
                { offset: 1, color: t.lineGradBot },
              ]),
            },
            markLine: {
              symbol: "none",
              silent: true,
              lineStyle: { color: t.ema, type: "dashed", width: 1, opacity: 0.8 },
              label: {
                formatter: "forward",
                color: t.ema,
                backgroundColor: t.tooltipBg,
                borderColor: t.tooltipBorder,
                borderWidth: 1,
                padding: [2, 5],
                borderRadius: 4,
                fontSize: 10,
                position: "insideEndTop",
              },
              data: [{ xAxis: strip.forward }],
            },
          },
        ],
      },
      true,
    );
  }, [strip, theme]);

  useEffect(() => {
    draw();
  }, [draw]);

  return (
    <div className="relative h-[260px] w-full">
      <div ref={ref} className="h-full w-full" />
      {!strip && !err && <p className="absolute inset-0 flex items-center justify-center text-xs text-soft">loading strip…</p>}
      {err && <p className="absolute inset-0 flex items-center justify-center text-xs text-down/80">strip unavailable: {err}</p>}
    </div>
  );
}
