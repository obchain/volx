import { VALUE_SCALE, CONFIDENCE_SCALE } from "./scale.js";
import { log } from "./log.js";

/** Subset of the VolX `/v1/index/{id}/latest` envelope the keeper reads. */
interface LatestResponse {
  index: string;
  value: number;
  ts: string;
  confidence: number;
}

export interface IndexQuote {
  /** index value scaled to VALUE_SCALE (1e8), ready for the oracle. */
  valueScaled: bigint;
  /** confidence scaled to CONFIDENCE_SCALE (1e6), clamped to [0, 1e6]. */
  confScaled: number;
  /** raw float value, for logging. */
  valueFloat: number;
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Fetch one index `latest` with exponential backoff. Throws after the last
 * attempt so the caller can skip this cycle without crashing the process. */
async function fetchLatest(apiUrl: string, id: "bvol" | "evol", attempts = 4): Promise<LatestResponse> {
  let delay = 500;
  for (let i = 1; i <= attempts; i++) {
    try {
      const ctl = AbortSignal.timeout(5_000);
      const res = await fetch(`${apiUrl}/v1/index/${id}/latest`, { signal: ctl });
      if (!res.ok) throw new Error(`HTTP ${res.status} for ${id}`);
      const body = (await res.json()) as LatestResponse;
      if (typeof body.value !== "number" || !Number.isFinite(body.value)) {
        throw new Error(`${id}: non-numeric value in response`);
      }
      return body;
    } catch (e) {
      if (i === attempts) throw e;
      log.warn(`api ${id} attempt ${i}/${attempts} failed: ${(e as Error).message}; retry in ${delay}ms`);
      await sleep(delay);
      delay *= 2;
    }
  }
  throw new Error("unreachable");
}

/** Convert a float index value/confidence into the oracle's fixed-point ints. */
export function toQuote(value: number, confidence: number): IndexQuote {
  const valueScaled = BigInt(Math.round(value * VALUE_SCALE));
  // Confidence is [0,1] from the engine; clamp + scale, and never exceed the
  // oracle's CONFIDENCE_SCALE cap (it reverts ConfidenceTooHigh otherwise).
  const confRaw = Number.isFinite(confidence) ? confidence : 0;
  const confScaled = Math.max(0, Math.min(CONFIDENCE_SCALE, Math.round(confRaw * CONFIDENCE_SCALE)));
  return { valueScaled, confScaled, valueFloat: value };
}

/** Fetch both indices and return oracle-ready quotes. */
export async function fetchQuotes(apiUrl: string): Promise<{ bvol: IndexQuote; evol: IndexQuote }> {
  const [b, e] = await Promise.all([fetchLatest(apiUrl, "bvol"), fetchLatest(apiUrl, "evol")]);
  return { bvol: toQuote(b.value, b.confidence), evol: toQuote(e.value, e.confidence) };
}
