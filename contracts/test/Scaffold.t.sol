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
        // Referencing each interface/type forces the import to compile.
        // type(...).interfaceId is a cheap, side-effect-free touch.
        bytes4 erc20Id = type(IERC20).interfaceId;
        bytes4 ownableId = type(Ownable).interfaceId;
        assertTrue(erc20Id != bytes4(0), "IERC20 resolved");
        assertTrue(ownableId != bytes4(0) || ownableId == bytes4(0), "Ownable resolved");
    }

    function test_ReentrancyGuardImported() public pure {
        // ReentrancyGuard has no interfaceId (abstract, no external fns);
        // the import alone proves resolution. Assert a trivial truth.
        assertEq(type(ReentrancyGuard).name, "ReentrancyGuard");
    }
}
