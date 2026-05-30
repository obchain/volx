// On-chain wiring for the VolX perp demo (Ethereum Sepolia). Addresses come
// from contracts/deployments/sepolia.json (the deploy artifact, #92). ABIs are
// trimmed to the members the frontend calls.
import type { Address } from "viem";

export const SEPOLIA_CHAIN_ID = 11155111;

export const ADDRESSES = {
  mockUSDC: "0x60137f8457Db371EE4092c5F6C8e389168C582F5",
  oracle: "0x1762841A53F396B6C55eFbbB662D17A3B7Fa4947",
  perp: "0x1BE8387f05d3556002683Fe0DE9131B15002b7fb",
} as const satisfies Record<string, Address>;

/** Public read RPC (no wallet needed for stats). Overridable at build time. */
export const READ_RPC_URL =
  process.env.NEXT_PUBLIC_SEPOLIA_RPC ?? "https://ethereum-sepolia-rpc.publicnode.com";

// Oracle fixed-point scales (mirror VolXOracle).
export const VALUE_SCALE = 100_000_000n; // 1e8
export const USDC_DECIMALS = 6;

/** VolXOracle.Index */
export const INDEX = { bvol: 0, evol: 1 } as const;
export type IndexKey = keyof typeof INDEX;

export const mockUsdcAbi = [
  { type: "function", name: "decimals", stateMutability: "pure", inputs: [], outputs: [{ type: "uint8" }] },
  { type: "function", name: "balanceOf", stateMutability: "view", inputs: [{ name: "a", type: "address" }], outputs: [{ type: "uint256" }] },
  { type: "function", name: "allowance", stateMutability: "view", inputs: [{ name: "o", type: "address" }, { name: "s", type: "address" }], outputs: [{ type: "uint256" }] },
  { type: "function", name: "approve", stateMutability: "nonpayable", inputs: [{ name: "s", type: "address" }, { name: "v", type: "uint256" }], outputs: [{ type: "bool" }] },
  { type: "function", name: "mint", stateMutability: "nonpayable", inputs: [{ name: "to", type: "address" }, { name: "amount", type: "uint256" }], outputs: [] },
  { type: "function", name: "faucet", stateMutability: "nonpayable", inputs: [], outputs: [] },
  { type: "function", name: "FAUCET_AMOUNT", stateMutability: "view", inputs: [], outputs: [{ type: "uint256" }] },
] as const;

export const oracleAbi = [
  {
    type: "function",
    name: "getPrice",
    stateMutability: "view",
    inputs: [{ name: "index", type: "uint8" }],
    outputs: [
      { name: "value", type: "uint64" },
      { name: "updatedAt", type: "uint64" },
      { name: "confidence", type: "uint32" },
    ],
  },
] as const;

export const perpAbi = [
  { type: "function", name: "decimals", stateMutability: "view", inputs: [], outputs: [{ type: "uint8" }] },
  { type: "function", name: "totalAssets", stateMutability: "view", inputs: [], outputs: [{ type: "uint256" }] },
  { type: "function", name: "reservedAssets", stateMutability: "view", inputs: [], outputs: [{ type: "uint256" }] },
  { type: "function", name: "availableAssets", stateMutability: "view", inputs: [], outputs: [{ type: "uint256" }] },
  { type: "function", name: "totalReserved", stateMutability: "view", inputs: [], outputs: [{ type: "uint256" }] },
  { type: "function", name: "totalSupply", stateMutability: "view", inputs: [], outputs: [{ type: "uint256" }] },
  { type: "function", name: "balanceOf", stateMutability: "view", inputs: [{ name: "a", type: "address" }], outputs: [{ type: "uint256" }] },
  { type: "function", name: "convertToShares", stateMutability: "view", inputs: [{ name: "assets", type: "uint256" }], outputs: [{ type: "uint256" }] },
  { type: "function", name: "convertToAssets", stateMutability: "view", inputs: [{ name: "shares", type: "uint256" }], outputs: [{ type: "uint256" }] },
  { type: "function", name: "nextPositionId", stateMutability: "view", inputs: [], outputs: [{ type: "uint256" }] },
  { type: "function", name: "MAX_LEVERAGE", stateMutability: "view", inputs: [], outputs: [{ type: "uint256" }] },
  {
    type: "function",
    name: "positions",
    stateMutability: "view",
    inputs: [{ name: "id", type: "uint256" }],
    outputs: [
      { name: "trader", type: "address" },
      { name: "index", type: "uint8" },
      { name: "isLong", type: "bool" },
      { name: "collateral", type: "uint256" },
      { name: "leverage", type: "uint256" },
      { name: "entryPrice", type: "uint256" },
      { name: "openedAt", type: "uint256" },
    ],
  },
  {
    type: "function",
    name: "positionValue",
    stateMutability: "view",
    inputs: [{ name: "id", type: "uint256" }],
    outputs: [
      { name: "pnl", type: "int256" },
      { name: "equity", type: "uint256" },
    ],
  },
  { type: "function", name: "isLiquidatable", stateMutability: "view", inputs: [{ name: "id", type: "uint256" }], outputs: [{ type: "bool" }] },
  { type: "function", name: "deposit", stateMutability: "nonpayable", inputs: [{ name: "assets", type: "uint256" }], outputs: [{ type: "uint256" }] },
  { type: "function", name: "withdraw", stateMutability: "nonpayable", inputs: [{ name: "shares", type: "uint256" }], outputs: [{ type: "uint256" }] },
  {
    type: "function",
    name: "openPosition",
    stateMutability: "nonpayable",
    inputs: [
      { name: "index", type: "uint8" },
      { name: "isLong", type: "bool" },
      { name: "collateral", type: "uint256" },
      { name: "leverage", type: "uint256" },
    ],
    outputs: [{ name: "id", type: "uint256" }],
  },
  { type: "function", name: "closePosition", stateMutability: "nonpayable", inputs: [{ name: "id", type: "uint256" }], outputs: [] },
  { type: "function", name: "liquidate", stateMutability: "nonpayable", inputs: [{ name: "id", type: "uint256" }], outputs: [] },
] as const;
