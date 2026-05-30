import { loadConfig } from "./config.js";
import { fetchQuotes } from "./api.js";
import { decide } from "./decide.js";
import { OracleClient } from "./chain.js";
import { log } from "./log.js";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function main(): Promise<void> {
  const cfg = loadConfig();
  const oracle = new OracleClient(cfg.rpcUrl, cfg.privateKey, cfg.oracleAddress);

  log.info("keeper starting", {
    oracle: cfg.oracleAddress,
    signer: oracle.signer,
    apiUrl: cfg.apiUrl,
    deviationBps: cfg.deviationBps,
    heartbeatMs: cfg.heartbeatMs,
    pollIntervalMs: cfg.pollIntervalMs,
    dryRun: cfg.dryRun,
    runOnce: cfg.runOnce,
  });

  // Sanity check: warn loudly if our signer is not the oracle's keeper — every
  // push would revert NotKeeper otherwise. Skipped in dry-run.
  if (!cfg.dryRun) {
    try {
      const onchainKeeper = await oracle.keeper();
      if (onchainKeeper.toLowerCase() !== oracle.signer.toLowerCase()) {
        log.warn("signer is not the oracle keeper; pushes will revert", {
          signer: oracle.signer,
          keeper: onchainKeeper,
        });
      }
    } catch (e) {
      log.warn("could not read oracle keeper", { err: (e as Error).message });
    }
  }

  // First cycle forces a push on a never-set oracle (decide() returns "init").
  do {
    try {
      await tick(cfg, oracle);
    } catch (e) {
      // tick already backs off internally on transient faults; this guards
      // anything unexpected so the loop never dies.
      log.error("tick failed", { err: (e as Error).message });
    }
    if (cfg.runOnce) break;
    await sleep(cfg.pollIntervalMs);
  } while (true);
}

async function tick(cfg: ReturnType<typeof loadConfig>, oracle: OracleClient): Promise<void> {
  const quotes = await fetchQuotes(cfg.apiUrl);

  // The oracle reverts ZeroValue on a 0 push; skip rather than burn gas on a
  // guaranteed-revert tx if the API ever returns a pathological zero.
  if (quotes.bvol.valueScaled === 0n || quotes.evol.valueScaled === 0n) {
    log.warn("zero index value from API, skipping cycle", {
      bvol: quotes.bvol.valueFloat,
      evol: quotes.evol.valueFloat,
    });
    return;
  }

  const chain = await oracle.readBoth();

  const d = decide({
    bvolNew: quotes.bvol.valueScaled,
    evolNew: quotes.evol.valueScaled,
    bvolChain: chain.bvol,
    evolChain: chain.evol,
    nowMs: Date.now(),
    deviationBps: cfg.deviationBps,
    heartbeatMs: cfg.heartbeatMs,
  });

  const base = {
    reason: d.reason,
    bvol: quotes.bvol.valueFloat,
    evol: quotes.evol.valueFloat,
    bvolBps: d.bvolBps,
    evolBps: d.evolBps,
  };

  if (!d.push) {
    log.info("skip", base);
    return;
  }
  if (cfg.dryRun) {
    log.info("push (dry-run, no tx)", base);
    return;
  }

  const hash = await oracle.pushBoth(quotes.bvol, quotes.evol);
  log.info("pushed updateBoth", { ...base, tx: hash });
}

main().catch((e) => {
  log.error("fatal", { err: (e as Error).message });
  process.exit(1);
});
