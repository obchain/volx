// Minimal VolXPerpV2 ABI — only what the order executor needs.
export const perpAbi = [
  { type: "function", name: "nextOrderId", stateMutability: "view", inputs: [], outputs: [{ type: "uint256" }] },
  {
    type: "function",
    name: "orders",
    stateMutability: "view",
    inputs: [{ name: "id", type: "uint256" }],
    outputs: [
      { name: "trader", type: "address" },
      { name: "kind", type: "uint8" },
      { name: "index", type: "uint8" },
      { name: "isLong", type: "bool" },
      { name: "collateral", type: "uint256" },
      { name: "leverage", type: "uint256" },
      { name: "triggerPrice", type: "uint256" },
      { name: "triggerAbove", type: "bool" },
      { name: "positionId", type: "uint256" },
    ],
  },
  { type: "function", name: "executeOrder", stateMutability: "nonpayable", inputs: [{ name: "id", type: "uint256" }], outputs: [{ type: "uint256" }] },
] as const;
