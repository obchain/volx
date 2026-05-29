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

    // NOTE: there is intentionally NO `balanceOf >= totalReserved` invariant —
    // `totalReserved` is leveraged notional (up to 10x collateral), which the
    // vault is not collateralized to. The reserve only gates LP withdrawals; it
    // is not a token-backing claim. Fee-only share-price monotonicity is asserted
    // inline in PerpHandler instead.

    /// @notice The sum of every LP's individually-redeemable assets never exceeds
    /// the vault's total assets. Each `convertToAssets(balanceOf)` floor-rounds, so
    /// the summed claims must stay within `totalAssets`; a broken share-math path
    /// that rounded up would let collective claims exceed the pot.
    function invariant_PerActorClaimsWithinAssets() public view {
        uint256 supply = perp.totalSupply();
        if (supply == 0) return;
        uint256 sumClaims;
        uint256 nActors = handler.actorsLength();
        for (uint256 i = 0; i < nActors; i++) {
            sumClaims += perp.convertToAssets(perp.balanceOf(handler.actors(i)));
        }
        assertLe(sumClaims, perp.totalAssets());
    }
}
