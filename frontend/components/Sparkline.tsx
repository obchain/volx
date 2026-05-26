// Inline SVG sparkline. Used on the landing page (#26). lightweight-charts
// is reserved for the full chart page (#27) — overkill for a tiny inline
// trend display, both in bundle weight and DOM cost per card.

interface SparklineProps {
  values: number[];
  width?: number;
  height?: number;
  className?: string;
}

export function Sparkline({ values, width = 240, height = 56, className }: SparklineProps) {
  if (values.length < 2) {
    return <svg width={width} height={height} className={className} aria-hidden="true" />;
  }

  const min = Math.min(...values);
  const max = Math.max(...values);
  const range = max - min || 1;
  const step = width / (values.length - 1);

  const points = values
    .map((v, i) => {
      const x = i * step;
      const y = height - ((v - min) / range) * height;
      return `${x.toFixed(2)},${y.toFixed(2)}`;
    })
    .join(" ");

  const last = values[values.length - 1];
  const first = values[0];
  const up = last >= first;

  return (
    <svg
      width={width}
      height={height}
      className={className}
      role="img"
      aria-label="sparkline trend last 1h"
    >
      <polyline
        fill="none"
        stroke={up ? "rgb(74, 222, 128)" : "rgb(248, 113, 113)"}
        strokeWidth={1.5}
        points={points}
      />
    </svg>
  );
}
