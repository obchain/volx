// Shared API contracts + HTTP base. Mirrors the Go API wire shapes in
// `api/internal/handlers/index.go` (REST envelopes) and
// `api/internal/stream/hub.go` (WS frame shape).

// REST goes through the Next.js rewrite proxy (see next.config.ts) so
// it stays same-origin without API-side CORS. WS bypasses the proxy
// (rewrites do not handle the upgrade) and connects directly. Override
// with `NEXT_PUBLIC_API_BASE` if the API is on a different host than
// localhost:8080.
const PUBLIC_API_BASE = process.env.NEXT_PUBLIC_API_BASE ?? "http://localhost:8080";

export const API_BASE = "";

export const WS_URL = (() => {
  const base = PUBLIC_API_BASE.replace(/^http/, "ws");
  return `${base}/v1/stream`;
})();

export type IndexId = "bvol" | "evol";

// REST GET /v1/index/{id}/latest
export interface LatestResponse {
  index: string;
  value: number;
  ts: string;
  confidence: number;
  source_strip_hash: string;
  next_update_eta_seconds: number;
}

// REST GET /v1/index/{id}/history
// Wire shape per `api/internal/handlers/index.go` IndexHistory.
export interface HistoryBar {
  ts: string;
  open: number;
  high: number;
  low: number;
  close: number;
  count: number;
  avg_confidence: number;
}

export interface HistoryResponse {
  index: string;
  interval: string;
  order: "oldest_first";
  bars: HistoryBar[];
}

// WS server -> client (`type: "tick"`).
export interface TickFrame {
  type: "tick";
  channel: IndexId;
  value: number;
  ts: number; // Unix epoch milliseconds (PRD §6)
  confidence: number;
}

export interface ErrorFrame {
  type: "error";
  code: string;
  message: string;
}

export type WsFrame = TickFrame | ErrorFrame;

export async function fetchLatest(id: IndexId): Promise<LatestResponse> {
  const r = await fetch(`${API_BASE}/v1/index/${id}/latest`, {
    cache: "no-store",
  });
  if (!r.ok) throw new Error(`latest ${id}: ${r.status}`);
  return (await r.json()) as LatestResponse;
}

export async function fetchHistory(
  id: IndexId,
  interval: "5m" | "1h" | "1d",
  limit: number,
): Promise<HistoryResponse> {
  const url = `${API_BASE}/v1/index/${id}/history?interval=${interval}&limit=${limit}`;
  const r = await fetch(url, { cache: "no-store" });
  if (!r.ok) throw new Error(`history ${id}: ${r.status}`);
  return (await r.json()) as HistoryResponse;
}
