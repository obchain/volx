// Inline SVG sparkline with gradient area fill. Used on the landing
// IndexCards (#26). Lightweight-charts / ECharts would be overkill for
// a tiny inline trend display, both in bundle weight and DOM cost.

interface SparklineProps {
  values: number[];
  width?: number;
  height?: number;
  className?: string;
  // When `tone === "accent"` the curve renders in the brand cyan rather
  // than the semantic up/down pair. Used in places where the sparkline
  // is decorative scaffolding, not direction signalling.
  tone?: "directional" | "accent";
}

export function Sparkline({
  values,
  width = 240,
  height = 64,
  className,
  tone = "directional",
}: SparklineProps) {
  if (values.length < 2) {
    return <svg width={width} height={height} className={className} aria-hidden="true" />;
  }

  const min = Math.min(...values);
  const max = Math.max(...values);
  const range = max - min || 1;
  const step = width / (values.length - 1);

  const STROKE = 1.75;
  const top = STROKE / 2;
  const drawH = height - STROKE;

  const points = values.map((v, i) => {
    const x = i * step;
    const y = top + drawH - ((v - min) / range) * drawH;
    return { x, y };
  });

  const pathData = points
    .map((p, i) => `${i === 0 ? "M" : "L"} ${p.x.toFixed(2)} ${p.y.toFixed(2)}`)
    .join(" ");

  // Close the path back along the bottom for the area fill.
  const areaPath = `${pathData} L ${(points[points.length - 1]?.x ?? 0).toFixed(2)} ${height} L 0 ${height} Z`;

  const last = values[values.length - 1];
  const first = values[0];
  const up = last >= first;

  const stroke =
    tone === "accent"
      ? "var(--accent)"
      : up
        ? "var(--up)"
        : "var(--down)";
  const fillStart =
    tone === "accent"
      ? "var(--accent-glow)"
      : up
        ? "rgba(74, 222, 128, 0.28)"
        : "rgba(248, 113, 113, 0.28)";

  const gradId = `volx-spark-grad-${Math.random().toString(36).slice(2, 8)}`;

  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      preserveAspectRatio="none"
      className={className}
      role="img"
      aria-label="sparkline trend"
    >
      <defs>
        <linearGradient id={gradId} x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={fillStart} />
          <stop offset="100%" stopColor="transparent" />
        </linearGradient>
      </defs>
      <path d={areaPath} fill={`url(#${gradId})`} />
      <path d={pathData} fill="none" stroke={stroke} strokeWidth={STROKE} strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}
