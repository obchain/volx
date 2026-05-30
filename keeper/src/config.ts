import { readFileSync } from "node:fs";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { type Address, isAddress } from "viem";

const here = dirname(fileURLToPath(import.meta.url));

export interface Config {
  rpcUrl: string;
  privateKey: `0x${string}`;
  oracleAddress: Address;
  apiUrl: string;
  pollIntervalMs: number;
  deviationBps: number;
  heartbeatMs: number;
  dryRun: boolean;
  runOnce: boolean;
}

function req(name: string): string {
  const v = process.env[name];
  if (!v || v.trim() === "") throw new Error(`missing required env ${name}`);
  return v.trim();
}

function num(name: string, fallback: number, min = 0): number {
  const v = process.env[name];
  if (v === undefined || v.trim() === "") return fallback;
  const n = Number(v);
  if (!Number.isFinite(n) || n < min) throw new Error(`env ${name} must be a number >= ${min}, got "${v}"`);
  return n;
}

function bool(name: string): boolean {
  const v = (process.env[name] ?? "").trim().toLowerCase();
  return v === "1" || v === "true" || v === "yes";
}

/** Resolve the oracle address: explicit ORACLE_ADDRESS wins, else the
 * `oracle` field of the deploy artifact. */
function resolveOracle(): Address {
  const explicit = process.env.ORACLE_ADDRESS?.trim();
  if (explicit) {
    if (!isAddress(explicit)) throw new Error(`ORACLE_ADDRESS is not a valid address: ${explicit}`);
    return explicit;
  }
  const path = resolve(here, "..", process.env.DEPLOYMENTS_PATH ?? "../contracts/deployments/sepolia.json");
  let parsed: { oracle?: string };
  try {
    parsed = JSON.parse(readFileSync(path, "utf8"));
  } catch (e) {
    throw new Error(`could not read deployments file at ${path}: ${(e as Error).message}. Set ORACLE_ADDRESS to skip.`);
  }
  if (!parsed.oracle || !isAddress(parsed.oracle)) {
    throw new Error(`deployments file ${path} has no valid "oracle" address`);
  }
  return parsed.oracle;
}

export function loadConfig(): Config {
  const pk = req("PRIVATE_KEY");
  const privateKey = (pk.startsWith("0x") ? pk : `0x${pk}`) as `0x${string}`;
  if (!/^0x[0-9a-fA-F]{64}$/.test(privateKey)) throw new Error("PRIVATE_KEY must be 32 bytes hex");

  return {
    rpcUrl: req("SEPOLIA_RPC_URL"),
    privateKey,
    oracleAddress: resolveOracle(),
    apiUrl: (process.env.VOLX_API_URL ?? "http://localhost:8090").replace(/\/$/, ""),
    pollIntervalMs: num("POLL_INTERVAL_MS", 60_000, 1_000),
    deviationBps: num("DEVIATION_BPS", 50, 1),
    heartbeatMs: num("HEARTBEAT_MS", 1_800_000, 1_000),
    dryRun: bool("DRY_RUN"),
    runOnce: bool("RUN_ONCE"),
  };
}
