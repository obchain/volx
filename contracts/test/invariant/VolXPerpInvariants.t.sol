// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Test } from "forge-std/Test.sol";
import { MockUSDC } from "../../src/MockUSDC.sol";
import { VolXOracle } from "../../src/VolXOracle.sol";
import { VolXPerp } from "../../src/VolXPerp.sol";
import { PerpHandler } from "./PerpHandler.sol";

/// @notice Stateful invariant suite for the perp. The handler drives random
/// deposit/withdraw/open/close/liquidate/price sequences; these properties must
/// hold in every reachable state.
contract VolXPerpInvariantsTest is Test {
    MockUSDC internal usdc;
    VolXOracle internal oracle;
    VolXPerp internal perp;
    PerpHandler internal handler;

    function setUp() public {
        vm.warp(1_700_000_000);
        usdc = new MockUSDC();
        perp = new VolXPerp(usdc, new VolXOracle(address(this)));
        oracle = perp.oracle();

        handler = new PerpHandler(usdc, oracle, perp);
        // Hand the keeper role to the handler so it can push prices.
        oracle.setKeeper(address(handler));
        // Seed both feeds so opens don't all revert on unset price.
        // (handler is keeper now, so prank it.)
        vm.startPrank(address(handler));
        oracle.updateBoth(60e8, 1e6, 80e8, 1e6);
        vm.stopPrank();

        targetContract(address(handler));
    }

    /// @notice The vault's real token balance always covers its accounting.
    /// `_totalAssets` excludes held trader collateral + donations, so the ERC20
    /// balance is always >= totalAssets. If this breaks, LPs cannot be paid.
    function invariant_TokenBalanceCoversAccounting() public view {
        assertGe(usdc.balanceOf(address(perp)), perp.totalAssets());
    }

    /// @notice `totalReserved` always equals the summed notional of every still-
    /// open position — proves reserve accounting never desyncs across the
    /// open/close/liquidate lifecycle.
    function invariant_ReservedEqualsOpenNotional() public view {
        uint256 n = handler.openedIdsLength();
        uint256 sum;
        for (uint256 i = 0; i < n; i++) {
            uint256 id = handler.openedIds(i);
            (address trader,,, uint256 collateral, uint256 leverage,,) = perp.positions(id);
            if (trader != address(0)) sum += collateral * leverage;
        }
        assertEq(sum, perp.totalReserved());
    }

    /// @notice Withdrawable (available) collateral never exceeds total assets.
    function invariant_AvailableNeverExceedsAssets() public view {
        assertLe(perp.availableAssets(), perp.totalAssets());
    }

    /// @notice LP share supply collectively redeems to at most total assets
    /// (share price is bounded by the vault's accounted collateral).
    function invariant_SharesRedeemWithinAssets() public view {
        uint256 supply = perp.totalSupply();
        if (supply == 0) return;
        assertLe(perp.convertToAssets(supply), perp.totalAssets());
    }
}
