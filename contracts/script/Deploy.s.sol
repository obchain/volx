// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Script } from "forge-std/Script.sol";
import { console2 } from "forge-std/console2.sol";
import { MockUSDC } from "../src/MockUSDC.sol";
import { VolXOracle } from "../src/VolXOracle.sol";
import { VolXPerp } from "../src/VolXPerp.sol";

/// @title Deploy
/// @notice One-shot Sepolia deploy: MockUSDC -> VolXOracle -> VolXPerp, with the
/// deployer wired as oracle keeper and the LP vault seeded with demo liquidity.
/// Writes `deployments/sepolia.json` for the keeper service and frontend to read.
///
/// @dev Run (dry):     forge script script/Deploy.s.sol --rpc-url "$SEPOLIA_RPC_URL"
///      Run (live):    forge script script/Deploy.s.sol --rpc-url "$SEPOLIA_RPC_URL" \
///                       --broadcast --verify --etherscan-api-key "$ETHERSCAN_API_KEY"
///
/// Env (from `.secrets/sepolia.env`, never hardcoded):
///   PRIVATE_KEY  — deployer key; also becomes the oracle keeper.
///   SEED_USDC    — optional whole-token vault seed (default 200_000 mUSDC).
contract Deploy is Script {
    /// @dev Default LP seed if `SEED_USDC` is unset: 200,000 mUSDC. Kept modest
    /// because MockUSDC caps each mint at 10k, so the seed is minted in chunks —
    /// 200k = 20 mint txs, deep enough for the demo while staying gas-frugal.
    uint256 internal constant DEFAULT_SEED_TOKENS = 200_000;

    function run() external {
        uint256 pk = vm.envUint("PRIVATE_KEY");
        address deployer = vm.addr(pk);

        // Whole-token seed -> base units (MockUSDC is 6dp). `SEED_USDC` is a
        // WHOLE-TOKEN count, not base units; cap it so a base-unit value passed by
        // mistake can't spin the chunked mint loop into millions of iterations.
        uint256 seedTokens = vm.envOr("SEED_USDC", DEFAULT_SEED_TOKENS);
        require(seedTokens <= 1_000_000, "SEED_USDC is whole tokens (<= 1,000,000)");
        uint256 seedAssets = seedTokens * 1e6;

        // Captured before broadcast: a safe floor the keeper (#93) can scan events
        // from. Read after stopBroadcast it would be the simulation tip, which can
        // sit past the block the seeding Deposit lands in.
        uint256 startBlock = block.number;

        console2.log("Deployer        :", deployer);
        console2.log("Deployer balance:", deployer.balance);
        console2.log("Seed (mUSDC)    :", seedTokens);

        vm.startBroadcast(pk);

        // 1. Collateral token.
        MockUSDC usdc = new MockUSDC();

        // 2. Oracle — deployer is the keeper (per demo config).
        VolXOracle oracle = new VolXOracle(deployer);

        // 3. Perp vault over (collateral, oracle).
        VolXPerp perp = new VolXPerp(usdc, oracle);

        // 4. Seed LP liquidity. MockUSDC caps each mint at MAX_MINT_PER_CALL, so
        //    mint in chunks up to the requested seed, then deposit it all.
        if (seedAssets > 0) {
            uint256 cap = usdc.MAX_MINT_PER_CALL();
            uint256 remaining = seedAssets;
            while (remaining > 0) {
                uint256 chunk = remaining > cap ? cap : remaining;
                usdc.mint(deployer, chunk);
                remaining -= chunk;
            }
            usdc.approve(address(perp), seedAssets);
            perp.deposit(seedAssets);
        }

        vm.stopBroadcast();

        console2.log("MockUSDC        :", address(usdc));
        console2.log("VolXOracle      :", address(oracle));
        console2.log("VolXPerp        :", address(perp));
        console2.log("Vault totalAssets:", perp.totalAssets());

        _writeDeployments(
            address(usdc), address(oracle), address(perp), deployer, seedAssets, startBlock
        );
    }

    /// @dev Serialize the addresses + metadata to `deployments/sepolia.json`.
    function _writeDeployments(
        address usdc,
        address oracle,
        address perp,
        address keeper,
        uint256 seedAssets,
        uint256 deployedBlock
    ) internal {
        string memory obj = "deployment";
        vm.serializeUint(obj, "chainId", block.chainid);
        vm.serializeUint(obj, "deployedBlock", deployedBlock);
        vm.serializeAddress(obj, "deployer", keeper);
        vm.serializeAddress(obj, "keeper", keeper);
        vm.serializeAddress(obj, "mockUSDC", usdc);
        vm.serializeAddress(obj, "oracle", oracle);
        vm.serializeUint(obj, "seedAssets", seedAssets);
        string memory out = vm.serializeAddress(obj, "perp", perp);

        string memory path = "deployments/sepolia.json";
        vm.writeJson(out, path);
        console2.log("Wrote", path);
    }
}
