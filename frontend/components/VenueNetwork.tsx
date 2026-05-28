"use client";

// Animated SVG showing the multi-venue blend: three venue nodes
// (Deribit · OKX · Bybit) feed pulses into a central VolX node. Pulses
// travel along the connection lines on a stagger so the eye reads
// "data continuously converging" rather than "synchronous flash."
//
// All SVG + CSS. No three.js, no canvas — keeps SSR happy and bundle
// weight ~negligible.

const CENTER_X = 400;
const CENTER_Y = 200;
const NODES = [
  { id: "deribit", label: "deribit", x: 90, y: 100, delay: "0s" },
  { id: "okx", label: "okx", x: 90, y: 300, delay: "0.7s" },
  { id: "bybit", label: "bybit", x: 90, y: 200, delay: "1.4s" },
] as const;

export function VenueNetwork() {
  return (
    <div className="relative mx-auto w-full max-w-2xl">
      <svg
        viewBox="0 0 800 400"
        className="h-auto w-full"
        role="img"
        aria-label="VolX multi-venue blend network — three venues converging on the central index"
      >
        <defs>
          <linearGradient id="volx-line" x1="0%" y1="0%" x2="100%" y2="0%">
            <stop offset="0%" stopColor="var(--accent)" stopOpacity="0.2" />
            <stop offset="100%" stopColor="var(--accent)" stopOpacity="0.7" />
          </linearGradient>
          <radialGradient id="volx-core-glow" cx="50%" cy="50%" r="50%">
            <stop offset="0%" stopColor="var(--accent)" stopOpacity="0.45" />
            <stop offset="60%" stopColor="var(--accent)" stopOpacity="0.12" />
            <stop offset="100%" stopColor="var(--accent)" stopOpacity="0" />
          </radialGradient>
          <radialGradient id="volx-node-glow" cx="50%" cy="50%" r="50%">
            <stop offset="0%" stopColor="var(--accent)" stopOpacity="0.5" />
            <stop offset="100%" stopColor="var(--accent)" stopOpacity="0" />
          </radialGradient>
        </defs>

        {/* Central glow */}
        <circle cx={CENTER_X} cy={CENTER_Y} r="150" fill="url(#volx-core-glow)" />

        {/* Connection lines */}
        {NODES.map((n) => (
          <line
            key={`line-${n.id}`}
            x1={n.x}
            y1={n.y}
            x2={CENTER_X}
            y2={CENTER_Y}
            stroke="url(#volx-line)"
            strokeWidth="1.5"
            strokeDasharray="4 6"
            className="volx-line"
          />
        ))}

        {/* Animated pulses traveling along each line */}
        {NODES.map((n) => (
          <g key={`pulse-${n.id}`}>
            <circle
              r="5"
              fill="var(--accent-strong)"
              className="volx-pulse-dot"
              style={
                {
                  // CSS custom props for the keyframe interpolation
                  "--from-x": `${n.x}px`,
                  "--from-y": `${n.y}px`,
                  "--to-x": `${CENTER_X}px`,
                  "--to-y": `${CENTER_Y}px`,
                  animationDelay: n.delay,
                } as React.CSSProperties
              }
            />
          </g>
        ))}

        {/* Venue nodes */}
        {NODES.map((n) => (
          <g key={`node-${n.id}`}>
            {/* Outer pulse ring */}
            <circle
              cx={n.x}
              cy={n.y}
              r="22"
              fill="url(#volx-node-glow)"
              className="volx-ring"
              style={{ animationDelay: n.delay }}
            />
            {/* Core dot */}
            <circle cx={n.x} cy={n.y} r="7" fill="var(--accent)" />
            <circle
              cx={n.x}
              cy={n.y}
              r="11"
              fill="none"
              stroke="var(--accent-strong)"
              strokeWidth="1.5"
              opacity="0.6"
            />
            {/* Label */}
            <text
              x={n.x - 32}
              y={n.y + 4}
              textAnchor="end"
              className="fill-foreground font-mono text-[13px] font-semibold"
            >
              {n.label}
            </text>
          </g>
        ))}

        {/* Central VolX node */}
        <g>
          <circle
            cx={CENTER_X}
            cy={CENTER_Y}
            r="44"
            fill="var(--background)"
            stroke="var(--accent)"
            strokeWidth="1.5"
          />
          <circle
            cx={CENTER_X}
            cy={CENTER_Y}
            r="56"
            fill="none"
            stroke="var(--accent)"
            strokeWidth="1"
            opacity="0.35"
            className="volx-core-ring"
          />
          <text
            x={CENTER_X}
            y={CENTER_Y - 4}
            textAnchor="middle"
            className="fill-foreground text-[15px] font-bold tracking-wider"
          >
            VolX
          </text>
          <text
            x={CENTER_X}
            y={CENTER_Y + 14}
            textAnchor="middle"
            className="fill-accent text-[9px] font-medium uppercase tracking-[0.18em]"
          >
            median blend
          </text>
        </g>
      </svg>

      <style jsx>{`
        :global(.volx-line) {
          animation: volx-line-flow 4s linear infinite;
        }
        :global(.volx-pulse-dot) {
          transform: translate(var(--from-x), var(--from-y));
          animation: volx-travel 3.4s ease-in-out infinite;
        }
        :global(.volx-ring) {
          transform-origin: center center;
          transform-box: fill-box;
          animation: volx-ring-pulse 2.8s ease-in-out infinite;
        }
        :global(.volx-core-ring) {
          transform-origin: ${CENTER_X}px ${CENTER_Y}px;
          animation: volx-core-pulse 3.2s ease-out infinite;
        }

        @keyframes volx-line-flow {
          to {
            stroke-dashoffset: -20;
          }
        }
        @keyframes volx-travel {
          0% {
            transform: translate(var(--from-x), var(--from-y));
            opacity: 0;
          }
          15% {
            opacity: 1;
          }
          85% {
            opacity: 1;
          }
          100% {
            transform: translate(var(--to-x), var(--to-y));
            opacity: 0;
          }
        }
        @keyframes volx-ring-pulse {
          0%,
          100% {
            transform: scale(0.85);
            opacity: 0.4;
          }
          50% {
            transform: scale(1.3);
            opacity: 0.9;
          }
        }
        @keyframes volx-core-pulse {
          0% {
            transform: scale(1);
            opacity: 0.55;
          }
          80%,
          100% {
            transform: scale(1.35);
            opacity: 0;
          }
        }
      `}</style>
    </div>
  );
}
