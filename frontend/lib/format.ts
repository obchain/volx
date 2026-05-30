import { formatUnits, parseUnits } from "viem";
import { USDC_DECIMALS, VALUE_SCALE } from "./contracts";

/** Format a 6dp USDC base-unit amount as a human string (e.g. "1,234.50"). */
export function fmtUsdc(v: bigint, dp = 2): string {
  const n = Number(formatUnits(v, USDC_DECIMALS));
  return n.toLocaleString("en-US", { minimumFractionDigits: dp, maximumFractionDigits: dp });
}

/** Parse a user-entered token amount into 6dp base units. Throws on garbage. */
export function parseUsdc(s: string): bigint {
  return parseUnits(s as `${number}`, USDC_DECIMALS);
}

/** Format an oracle value (1e8 fixed-point) as a vol number (e.g. "61.50"). */
export function fmtPrice(v: bigint): string {
  return (Number(v) / Number(VALUE_SCALE)).toLocaleString("en-US", {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  });
}

/** Signed USDC pnl with a leading +/-. */
export function fmtPnl(v: bigint): string {
  const sign = v < 0n ? "-" : "+";
  const abs = v < 0n ? -v : v;
  return `${sign}${fmtUsdc(abs)}`;
}

/** Short 0x1234…abcd address. */
export function shortAddr(a: string): string {
  return `${a.slice(0, 6)}…${a.slice(-4)}`;
}
