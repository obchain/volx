// Minimal VolXOracle ABI — only the members the keeper touches. Index enum:
// 0 = BVOL, 1 = EVOL (matches VolXOracle.Index).
export const oracleAbi = [
  {
    type: "function",
    name: "updateBoth",
    stateMutability: "nonpayable",
    inputs: [
      { name: "bvol", type: "uint64" },
      { name: "bvolConf", type: "uint32" },
      { name: "evol", type: "uint64" },
      { name: "evolConf", type: "uint32" },
    ],
    outputs: [],
  },
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
  {
    type: "function",
    name: "keeper",
    stateMutability: "view",
    inputs: [],
    outputs: [{ name: "", type: "address" }],
  },
] as const;

export const INDEX = { BVOL: 0, EVOL: 1 } as const;
