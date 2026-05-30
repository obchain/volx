import { test } from "node:test";
import assert from "node:assert/strict";
import { decide, deviationBpsOf, type DecisionInput } from "./decide.js";
import { toQuote } from "./api.js";

const base: DecisionInput = {
  bvolNew: 60_00000000n,
  evolNew: 80_00000000n,
  bvolChain: { value: 60_00000000n, updatedAt: 1000n },
  evolChain: { value: 80_00000000n, updatedAt: 1000n },
  nowMs: 1_000_000 + 1_000, // 1s after updatedAt (1000s = 1_000_000ms); tests override as needed
  deviationBps: 50,
  heartbeatMs: 1_800_000,
};

test("deviationBpsOf: zero prev returns 0", () => {
  assert.equal(deviationBpsOf(0n, 100n), 0);
});

test("deviationBpsOf: 1% move is 100 bps", () => {
  assert.equal(deviationBpsOf(100_00000000n, 101_00000000n), 100);
});

test("init: pushes when either feed never set", () => {
  const d = decide({ ...base, bvolChain: { value: 0n, updatedAt: 0n } });
  assert.equal(d.push, true);
  assert.equal(d.reason, "init");
});

test("no-change: identical values within heartbeat -> skip", () => {
  const d = decide({ ...base, nowMs: 1000 * 1000 + 1000 }); // 1s after updatedAt
  assert.equal(d.push, false);
  assert.equal(d.reason, "no-change");
});

test("deviation: BVOL +0.5% triggers a push", () => {
  const d = decide({ ...base, bvolNew: 60_30000000n, nowMs: 1000 * 1000 + 1000 });
  assert.equal(d.push, true);
  assert.equal(d.reason, "deviation");
  assert.equal(d.bvolBps, 50);
});

test("deviation: +0.49% stays below threshold -> skip", () => {
  // 60.00 -> 60.294 = 49 bps
  const d = decide({ ...base, bvolNew: 60_29400000n, nowMs: 1000 * 1000 + 1000 });
  assert.equal(d.push, false);
});

test("heartbeat: no move but stale -> push", () => {
  const d = decide({ ...base, nowMs: 1000 * 1000 + 1_800_001 });
  assert.equal(d.push, true);
  assert.equal(d.reason, "heartbeat");
});

test("toQuote: scales value to 1e8 and clamps confidence to 1e6", () => {
  const q = toQuote(60.5, 0.95);
  assert.equal(q.valueScaled, 60_50000000n);
  assert.equal(q.confScaled, 950_000);

  const over = toQuote(1, 5); // confidence above 1.0 must clamp
  assert.equal(over.confScaled, 1_000_000);
});
