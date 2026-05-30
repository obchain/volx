/** On-chain price snapshot for one index (from VolXOracle.getPrice). */
export interface OnChain {
  value: bigint; // at VALUE_SCALE
  updatedAt: bigint; // unix seconds; 0 = never set
}

export interface DecisionInput {
  bvolNew: bigint;
  evolNew: bigint;
  bvolChain: OnChain;
  evolChain: OnChain;
  nowMs: number;
  deviationBps: number;
  heartbeatMs: number;
}

export interface Decision {
  push: boolean;
  reason: "init" | "deviation" | "heartbeat" | "no-change";
  bvolBps: number; // |new-old|/old in bps; 0 when old is 0
  evolBps: number;
}

/** Absolute deviation of `next` vs `prev` in basis points. 0 if prev is 0. */
export function deviationBpsOf(prev: bigint, next: bigint): number {
  if (prev === 0n) return 0;
  const diff = next > prev ? next - prev : prev - next;
  return Number((diff * 10_000n) / prev);
}

/** Decide whether to push, Chainlink-style: push on first sight, on either
 * index deviating past the threshold, or once the heartbeat elapses. */
export function decide(i: DecisionInput): Decision {
  const bvolBps = deviationBpsOf(i.bvolChain.value, i.bvolNew);
  const evolBps = deviationBpsOf(i.evolChain.value, i.evolNew);

  // Never set on-chain yet → always push.
  if (i.bvolChain.updatedAt === 0n || i.evolChain.updatedAt === 0n) {
    return { push: true, reason: "init", bvolBps, evolBps };
  }

  if (bvolBps >= i.deviationBps || evolBps >= i.deviationBps) {
    return { push: true, reason: "deviation", bvolBps, evolBps };
  }

  // Heartbeat keys on the staler of the two feeds.
  const oldestSec = i.bvolChain.updatedAt < i.evolChain.updatedAt ? i.bvolChain.updatedAt : i.evolChain.updatedAt;
  const ageMs = i.nowMs - Number(oldestSec) * 1000;
  if (ageMs >= i.heartbeatMs) {
    return { push: true, reason: "heartbeat", bvolBps, evolBps };
  }

  return { push: false, reason: "no-change", bvolBps, evolBps };
}
