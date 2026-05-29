// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Test } from "forge-std/Test.sol";
import { IERC20 } from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import { Ownable } from "@openzeppelin/contracts/access/Ownable.sol";
import { ReentrancyGuard } from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

/// @notice Scaffold smoke test. Proves forge-std and the OpenZeppelin
/// remappings resolve and compile. No product contracts exist yet — those
/// land in #86 (MockUSDC), #87 (Oracle), #88-#90 (Perp). Delete or fold into
/// real suites once those arrive.
contract ScaffoldTest is Test {
    function test_RemappingsResolve() public pure {
        // Referencing each type's interfaceId forces the import to compile and
        // yields a non-zero selector XOR — a falsifiable proof of resolution.
        assertTrue(type(IERC20).interfaceId != bytes4(0), "IERC20 resolved");
        assertTrue(type(Ownable).interfaceId != bytes4(0), "Ownable resolved");
    }

    function test_ReentrancyGuardImported() public pure {
        // ReentrancyGuard has no interfaceId (abstract, no external fns);
        // the import alone proves resolution. Assert a trivial truth.
        assertEq(type(ReentrancyGuard).name, "ReentrancyGuard");
    }
}
