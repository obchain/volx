// Timeframe → REST history interval + limit. Bars math:
//   1m × 60   = 1 h
//   5m × 288  = 1 d
//   1h × 168  = 7 d
//   1h × 720  = 30 d
//   1d × 90   = 90 d
//   1d × 5000 = ~14 y (effectively "all" within the API's 10000 cap)

export const TIMEFRAMES = ["1h", "1d", "7d", "30d", "90d", "all"] as const;
export type Timeframe = (typeof TIMEFRAMES)[number];

export type HistoryInterval = "1m" | "5m" | "1h" | "1d";

export const DEFAULT_TIMEFRAME: Timeframe = "1d";

interface TimeframeSpec {
  interval: HistoryInterval;
  limit: number;
}

export const TIMEFRAME_SPEC: Record<Timeframe, TimeframeSpec> = {
  "1h": { interval: "1m", limit: 60 },
  "1d": { interval: "5m", limit: 288 },
  "7d": { interval: "1h", limit: 168 },
  "30d": { interval: "1h", limit: 720 },
  "90d": { interval: "1d", limit: 90 },
  all: { interval: "1d", limit: 5000 },
};
