// SVG arc ring showing the [0, 1] confidence value. The arc fills
// clockwise from 12 o'clock; the colour band shifts from the brand
// accent (high confidence) to the semantic down (low confidence) at
// the standard thresholds documented in METHODOLOGY.md §5.

interface ConfidenceRingProps {
  value: number | null;
  size?: number;
  thickness?: number;
}

export function ConfidenceRing({ value, size = 56, thickness = 4 }: ConfidenceRingProps) {
  const r = (size - thickness) / 2;
  const cx = size / 2;
  const cy = size / 2;
  const circumference = 2 * Math.PI * r;

  const v = value === null ? 0 : Math.max(0, Math.min(1, value));
  const dash = circumference * v;
  const gap = circumference - dash;

  const colour = ringColour(v);

  return (
    <div
      className="relative inline-flex items-center justify-center"
      style={{ width: size, height: size }}
      aria-label={value === null ? "confidence unavailable" : `confidence ${(v * 100).toFixed(0)}%`}
    >
      <svg width={size} height={size} className="-rotate-90">
        {/* track */}
        <circle
          cx={cx}
          cy={cy}
          r={r}
          fill="none"
          stroke="var(--border-subtle)"
          strokeWidth={thickness}
        />
        {/* progress */}
        {value !== null && (
          <circle
            cx={cx}
            cy={cy}
            r={r}
            fill="none"
            stroke={colour}
            strokeWidth={thickness}
            strokeLinecap="round"
            strokeDasharray={`${dash.toFixed(2)} ${gap.toFixed(2)}`}
            style={{ transition: "stroke-dasharray 600ms ease-out, stroke 600ms ease-out" }}
          />
        )}
      </svg>
      <div className="absolute inset-0 flex flex-col items-center justify-center">
        <span className="font-mono text-[10px] font-semibold tabular-nums leading-none text-foreground">
          {value === null ? "—" : v.toFixed(2)}
        </span>
        <span className="text-[8px] uppercase tracking-[0.18em] text-soft-2">conf</span>
      </div>
    </div>
  );
}

function ringColour(v: number) {
  if (v >= 0.85) return "var(--accent)";
  if (v >= 0.5) return "var(--accent)";
  if (v >= 0.25) return "#f59e0b";
  return "var(--down)";
}
