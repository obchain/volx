// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Script } from "forge-std/Script.sol";
import { console2 } from "forge-std/console2.sol";
import { MockUSDC } from "../src/MockUSDC.sol";
import { VolXOracle } from "../src/VolXOracle.sol";
import { VolXPerpV2 } from "../src/VolXPerpV2.sol";

/// @title DeployV2
/// @notice Deploys VolXPerpV2 against the EXISTING MockUSDC + VolXOracle (the
/// keeper already pushes fresh prices to that oracle), seeds its LP vault, and
/// records the address. The v1 perp is left as-is; the frontend repoints to v2.
///
/// Reads `deployments/sepolia.json` for the existing `mockUSDC` + `oracle`
/// addresses; writes `deployments/sepolia-v2.json` with the new perp.
contract DeployV2 is Script {
    uint256 internal constant DEFAULT_SEED_TOKENS = 200_000;

    function run() external {
        uint256 pk = vm.envUint("PRIVATE_KEY");
        address deployer = vm.addr(pk);

        string memory dep = vm.readFile("deployments/sepolia.json");
        address usdcAddr = vm.parseJsonAddress(dep, ".mockUSDC");
        address oracleAddr = vm.parseJsonAddress(dep, ".oracle");
        MockUSDC usdc = MockUSDC(usdcAddr);
        VolXOracle oracle = VolXOracle(oracleAddr);

        uint256 seedTokens = vm.envOr("SEED_USDC", DEFAULT_SEED_TOKENS);
        require(seedTokens <= 1_000_000, "SEED_USDC is whole tokens (<= 1,000,000)");
        uint256 seedAssets = seedTokens * 1e6;

        uint256 startBlock = block.number;
        console2.log("Deployer  :", deployer);
        console2.log("Reuse USDC:", usdcAddr);
        console2.log("Reuse Orac:", oracleAddr);
        console2.log("Seed mUSDC:", seedTokens);

        vm.startBroadcast(pk);

        VolXPerpV2 perp = new VolXPerpV2(usdc, oracle);

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

        console2.log("VolXPerpV2:", address(perp));
        console2.log("Vault TVL :", perp.totalAssets());

        string memory obj = "deploymentV2";
        vm.serializeUint(obj, "chainId", block.chainid);
        vm.serializeUint(obj, "deployedBlock", startBlock);
        vm.serializeAddress(obj, "deployer", deployer);
        vm.serializeAddress(obj, "keeper", deployer);
        vm.serializeAddress(obj, "mockUSDC", usdcAddr);
        vm.serializeAddress(obj, "oracle", oracleAddr);
        vm.serializeUint(obj, "seedAssets", seedAssets);
        string memory out = vm.serializeAddress(obj, "perp", address(perp));
        vm.writeJson(out, "deployments/sepolia-v2.json");
        console2.log("Wrote deployments/sepolia-v2.json");
    }
}
