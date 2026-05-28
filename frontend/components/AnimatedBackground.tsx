"use client";

// Full-bleed decorative backdrop:
//   - SVG grid that fades from accent on the left to nothing on the right
//   - Three slow-drifting blurred blobs (cyan, emerald, indigo) inspired by
//     Hyperliquid / Linear / Vercel gradient meshes
//   - Vignette dimming the edges so the foreground reads strongly
//
// All CSS-driven, no JS animation frame — cheap on CPU.

export function AnimatedBackground() {
  return (
    <div
      aria-hidden
      className="pointer-events-none fixed inset-0 -z-10 overflow-hidden"
      style={{ contain: "strict" }}
    >
      {/* Mesh blobs */}
      <div className="volx-blob volx-blob-1" />
      <div className="volx-blob volx-blob-2" />
      <div className="volx-blob volx-blob-3" />

      {/* Faint grid */}
      <svg
        className="absolute inset-0 h-full w-full opacity-[0.16]"
        xmlns="http://www.w3.org/2000/svg"
      >
        <defs>
          <pattern
            id="volx-grid"
            width="56"
            height="56"
            patternUnits="userSpaceOnUse"
          >
            <path
              d="M 56 0 L 0 0 0 56"
              fill="none"
              stroke="currentColor"
              strokeWidth="0.5"
              className="text-soft-2"
            />
          </pattern>
          <radialGradient id="volx-grid-fade" cx="50%" cy="0%" r="80%">
            <stop offset="0%" stopColor="white" stopOpacity="1" />
            <stop offset="100%" stopColor="white" stopOpacity="0" />
          </radialGradient>
          <mask id="volx-grid-mask">
            <rect width="100%" height="100%" fill="url(#volx-grid-fade)" />
          </mask>
        </defs>
        <rect width="100%" height="100%" fill="url(#volx-grid)" mask="url(#volx-grid-mask)" />
      </svg>

      {/* Edge vignette so foreground reads strongly */}
      <div
        className="absolute inset-0"
        style={{
          background:
            "radial-gradient(ellipse at center, transparent 0%, transparent 55%, var(--background) 95%)",
        }}
      />

      <style jsx>{`
        .volx-blob {
          position: absolute;
          border-radius: 9999px;
          filter: blur(80px);
          opacity: 0.55;
          will-change: transform;
        }
        .volx-blob-1 {
          width: 520px;
          height: 520px;
          left: -120px;
          top: -120px;
          background: var(--accent);
          animation: volx-drift-1 28s ease-in-out infinite alternate;
        }
        .volx-blob-2 {
          width: 600px;
          height: 600px;
          right: -150px;
          top: 80px;
          background: var(--up);
          opacity: 0.28;
          animation: volx-drift-2 34s ease-in-out infinite alternate;
        }
        .volx-blob-3 {
          width: 480px;
          height: 480px;
          left: 40%;
          top: 380px;
          background: var(--accent-strong);
          opacity: 0.18;
          animation: volx-drift-3 42s ease-in-out infinite alternate;
        }

        @keyframes volx-drift-1 {
          0% {
            transform: translate3d(0, 0, 0) scale(1);
          }
          100% {
            transform: translate3d(220px, 160px, 0) scale(1.15);
          }
        }
        @keyframes volx-drift-2 {
          0% {
            transform: translate3d(0, 0, 0) scale(1);
          }
          100% {
            transform: translate3d(-180px, 220px, 0) scale(0.92);
          }
        }
        @keyframes volx-drift-3 {
          0% {
            transform: translate3d(0, 0, 0) scale(1);
          }
          100% {
            transform: translate3d(-140px, -140px, 0) scale(1.2);
          }
        }
      `}</style>
    </div>
  );
}
