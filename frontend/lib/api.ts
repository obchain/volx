// Shared API contracts + HTTP base. Mirrors the Go API wire shapes in
// `api/internal/handlers/index.go` (REST envelopes) and
// `api/internal/stream/hub.go` (WS frame shape).
//
// Two surfaces, two transport stories:
//
// - REST always uses a same-origin relative path (`/v1/...`). The Next
//   dev/prod server rewrites that to the Go API target — see
//   `next.config.ts` (`API_PROXY_TARGET`, default `http://127.0.0.1:8090`).
//   No CORS plumbing needed.
//
// - WebSocket cannot ride the rewrite (the upgrade handshake bypasses
//   the rewrite layer) and connects directly. The host is configurable
//   via `NEXT_PUBLIC_API_BASE`, default `http://127.0.0.1:8090`. If the
//   API is reachable on a non-default host, both `API_PROXY_TARGET`
//   (build-time, server) and `NEXT_PUBLIC_API_BASE` (build-time, public)
//   must point at it.
const WS_API_BASE = process.env.NEXT_PUBLIC_API_BASE ?? "http://127.0.0.1:8090";

export const WS_URL = `${WS_API_BASE.replace(/^http/, "ws")}/v1/stream`;

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

// When the API is reached through an ngrok free tunnel, the agent serves
// an HTML interstitial warning page to browser-looking requests, which
// breaks JSON parsing. Sending this header (any value) skips it. Harmless
// against the direct local API. Forwarded upstream by the Next rewrite.
const REST_HEADERS = { "ngrok-skip-browser-warning": "true" } as const;

export async function fetchLatest(id: IndexId): Promise<LatestResponse> {
  const r = await fetch(`/v1/index/${id}/latest`, {
    cache: "no-store",
    headers: REST_HEADERS,
  });
  if (!r.ok) throw new Error(`latest ${id}: ${r.status}`);
  return (await r.json()) as LatestResponse;
}

export async function fetchHistory(
  id: IndexId,
  interval: "1m" | "5m" | "1h" | "1d",
  limit: number,
): Promise<HistoryResponse> {
  const url = `/v1/index/${id}/history?interval=${interval}&limit=${limit}`;
  const r = await fetch(url, { cache: "no-store", headers: REST_HEADERS });
  if (!r.ok) throw new Error(`history ${id}: ${r.status}`);
  return (await r.json()) as HistoryResponse;
}

// REST GET /v1/health
export interface HealthResponse {
  status: string;
  last_update_age_s: number;
  version: string;
}

export async function fetchHealth(): Promise<HealthResponse> {
  const r = await fetch(`/v1/health`, { cache: "no-store", headers: REST_HEADERS });
  if (!r.ok) throw new Error(`health: ${r.status}`);
  return (await r.json()) as HealthResponse;
}

// REST GET /v1/options/strip?index={id}
// One expiry's strip: forward + the [strike, q_usd, iv] quote rows that feed
// the variance integral. `near` is the front expiry; `next` the following one.
export interface StripExpiry {
  forward: number;
  k_zero: number;
  quotes: [number, number, number][]; // [strike, q_usd (option price), iv (decimal)]
}

export interface StripResponse {
  index_id: string;
  near?: StripExpiry;
  next?: StripExpiry;
}

export async function fetchStrip(id: IndexId): Promise<StripResponse> {
  const r = await fetch(`/v1/options/strip?index=${id}`, { cache: "no-store", headers: REST_HEADERS });
  if (!r.ok) throw new Error(`strip ${id}: ${r.status}`);
  return (await r.json()) as StripResponse;
}
