// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Test } from "forge-std/Test.sol";
import { MockUSDC } from "../src/MockUSDC.sol";
import { VolXOracle } from "../src/VolXOracle.sol";
import { VolXPerp } from "../src/VolXPerp.sol";

/// @notice Tests for the open/close + PnL settlement of VolXPerp (#89).
/// The test contract is the oracle keeper, so it pushes prices directly.
contract VolXPerpPositionsTest is Test {
    MockUSDC internal usdc;
    VolXOracle internal oracle;
    VolXPerp internal perp;

    address internal lp = makeAddr("lp");
    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");

    uint256 internal constant ONE = 1e6; // 1 mUSDC
    uint64 internal constant ENTRY = 60e8; // BVOL 60.0 at 1e8 scale
    uint32 internal constant CONF = 1e6;

    function setUp() public {
        vm.warp(1_700_000_000);
        usdc = new MockUSDC();
        oracle = new VolXOracle(address(this)); // this == keeper
        perp = new VolXPerp(usdc, oracle);

        _fund(lp, 2_000_000 * ONE);
        _fund(alice, 100_000 * ONE);
        _fund(bob, 100_000 * ONE);

        // Seed deep LP liquidity so the vault can pay any demo-sized win.
        vm.prank(lp);
        perp.deposit(1_000_000 * ONE);

        _setBvol(ENTRY);
    }

    function _fund(address who, uint256 amount) internal {
        deal(address(usdc), who, amount);
        vm.prank(who);
        usdc.approve(address(perp), type(uint256).max);
    }

    function _setBvol(uint64 v) internal {
        oracle.updateOne(VolXOracle.Index.BVOL, v, CONF);
    }

    function _open(address who, bool isLong, uint256 collateral, uint256 leverage)
        internal
        returns (uint256 id)
    {
        vm.prank(who);
        id = perp.openPosition(VolXOracle.Index.BVOL, isLong, collateral, leverage);
    }

    // --- open ---------------------------------------------------------------

    function test_OpenPullsCollateralAndRecordsEntry() public {
        uint256 balBefore = usdc.balanceOf(alice);
        uint256 taBefore = perp.totalAssets();

        uint256 id = _open(alice, true, 1000 * ONE, 5);

        // collateral 1000, openFee = 1000*5 * 10/10000 = 5 mUSDC, working = 995.
        uint256 openFee = 5 * ONE;
        uint256 working = 1000 * ONE - openFee;
        uint256 notional = working * 5;

        assertEq(usdc.balanceOf(alice), balBefore - 1000 * ONE);
        assertEq(perp.totalReserved(), notional);
        assertEq(perp.totalAssets(), taBefore + openFee); // open fee credited to vault

        (
            address trader,
            VolXOracle.Index index,
            bool isLong,
            uint256 collateral,
            uint256 leverage,
            uint256 entryPrice,
        ) = perp.positions(id);
        assertEq(trader, alice);
        assertEq(uint8(index), uint8(VolXOracle.Index.BVOL));
        assertTrue(isLong);
        assertEq(collateral, working);
        assertEq(leverage, 5);
        assertEq(entryPrice, ENTRY);
    }

    function test_OpenRevertsOnStaleOracle() public {
        vm.warp(block.timestamp + oracle.MAX_STALENESS() + 1);
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(
                VolXOracle.StalePrice.selector,
                VolXOracle.Index.BVOL,
                uint64(1_700_000_000),
                block.timestamp,
                oracle.MAX_STALENESS()
            )
        );
        perp.openPosition(VolXOracle.Index.BVOL, true, 1000 * ONE, 5);
    }

    function test_OpenRevertsWhenIndexNeverSet() public {
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(VolXOracle.PriceNeverSet.selector, VolXOracle.Index.EVOL)
        );
        perp.openPosition(VolXOracle.Index.EVOL, true, 1000 * ONE, 5);
    }

    function test_LeverageBoundsEnforced() public {
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(VolXPerp.InvalidLeverage.selector, 0));
        perp.openPosition(VolXOracle.Index.BVOL, true, 1000 * ONE, 0);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(VolXPerp.InvalidLeverage.selector, 11));
        perp.openPosition(VolXOracle.Index.BVOL, true, 1000 * ONE, 11);

        // 1x and 10x both allowed.
        _open(alice, true, 1000 * ONE, 1);
        _open(alice, true, 1000 * ONE, 10);
    }

    function test_ZeroCollateralReverts() public {
        vm.prank(alice);
        vm.expectRevert(VolXPerp.ZeroCollateral.selector);
        perp.openPosition(VolXOracle.Index.BVOL, true, 0, 5);
    }

    // --- close: PnL sign ----------------------------------------------------

    function test_LongProfitsWhenIndexRises() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        uint256 balAfterOpen = usdc.balanceOf(alice);

        _setBvol(66e8); // +10%

        vm.prank(alice);
        perp.closePosition(id);

        // working 995, notional 4975, pnl = 4975 * 0.1 = 497.5, closeFee = 4.975.
        uint256 expectedPayout = (995 * ONE) + (4975 * ONE / 10) - (4975 * ONE / 1000);
        assertEq(usdc.balanceOf(alice) - balAfterOpen, expectedPayout);
        assertEq(perp.totalReserved(), 0);
    }

    function test_ShortProfitsWhenIndexFalls() public {
        uint256 id = _open(alice, false, 1000 * ONE, 5);
        uint256 balAfterOpen = usdc.balanceOf(alice);

        _setBvol(54e8); // -10%

        vm.prank(alice);
        perp.closePosition(id);

        // Short with -10% move profits symmetrically to the long +10% case.
        uint256 expectedPayout = (995 * ONE) + (4975 * ONE / 10) - (4975 * ONE / 1000);
        assertEq(usdc.balanceOf(alice) - balAfterOpen, expectedPayout);
    }

    function test_LongLosesWhenIndexFalls() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        uint256 balAfterOpen = usdc.balanceOf(alice);
        uint256 taAfterOpen = perp.totalAssets();

        _setBvol(54e8); // -10% -> long loses

        vm.prank(alice);
        perp.closePosition(id);

        // pnl = -497.5, payout = 995 - 497.5 - closeFee(4.975) = 492.525.
        uint256 expectedPayout = (995 * ONE) - (4975 * ONE / 10) - (4975 * ONE / 1000);
        assertEq(usdc.balanceOf(alice) - balAfterOpen, expectedPayout);
        // Vault gains the trader's loss + close fee = working - payout.
        assertEq(perp.totalAssets() - taAfterOpen, (995 * ONE) - expectedPayout);
    }

    // --- close: loss cap ----------------------------------------------------

    function test_TraderLossCappedAtCollateral() public {
        uint256 id = _open(alice, true, 1000 * ONE, 10);
        uint256 balAfterOpen = usdc.balanceOf(alice);
        uint256 taAfterOpen = perp.totalAssets();

        // working 990, notional 9900. -20% move -> pnl = -1980 > collateral 990.
        _setBvol(48e8);

        vm.prank(alice);
        perp.closePosition(id);

        // Payout floored at 0; trader loses exactly the working collateral.
        assertEq(usdc.balanceOf(alice), balAfterOpen);
        assertEq(perp.totalAssets() - taAfterOpen, 990 * ONE); // vault gains all of it
        assertEq(perp.totalReserved(), 0);
    }

    // --- fees ---------------------------------------------------------------

    function test_CloseAtSamePriceChargesOnlyFees() public {
        uint256 taBeforeOpen = perp.totalAssets();
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        uint256 balAfterOpen = usdc.balanceOf(alice);

        // No price move: pnl = 0.
        vm.prank(alice);
        perp.closePosition(id);

        uint256 openFee = 5 * ONE;
        uint256 closeFee = 4975 * ONE / 1000; // 4.975
        // Trader gets back working - closeFee.
        assertEq(usdc.balanceOf(alice) - balAfterOpen, (995 * ONE) - closeFee);
        // Vault keeps both fees.
        assertEq(perp.totalAssets() - taBeforeOpen, openFee + closeFee);
    }

    // --- access / lifecycle -------------------------------------------------

    function test_OnlyOwnerCanClose() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        vm.prank(bob);
        vm.expectRevert(abi.encodeWithSelector(VolXPerp.NotPositionOwner.selector, bob, alice));
        perp.closePosition(id);
    }

    function test_CloseNonexistentReverts() public {
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(VolXPerp.PositionNotFound.selector, 42));
        perp.closePosition(42);
    }

    function test_CloseDeletesPosition() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        vm.prank(alice);
        perp.closePosition(id);
        (address trader,,,,,,) = perp.positions(id);
        assertEq(trader, address(0));
    }

    function test_CloseRevertsOnStaleOracle() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        // Capture oracle reads BEFORE the prank — an external call between
        // vm.prank and the target would consume the prank.
        uint256 maxStale = oracle.MAX_STALENESS();
        vm.warp(block.timestamp + maxStale + 1);
        uint256 nowTs = block.timestamp;
        bytes memory err = abi.encodeWithSelector(
            VolXOracle.StalePrice.selector,
            VolXOracle.Index.BVOL,
            uint64(1_700_000_000),
            nowTs,
            maxStale
        );

        vm.prank(alice);
        vm.expectRevert(err);
        perp.closePosition(id);
    }

    // --- positionValue view -------------------------------------------------

    function test_PositionValueReflectsLivePnL() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        _setBvol(66e8); // +10%

        (int256 pnl, uint256 equity) = perp.positionValue(id);
        int256 expectedPnl = 497_500_000; // +497.5 mUSDC
        assertEq(pnl, expectedPnl);
        assertEq(equity, (995 * ONE) + (4975 * ONE / 10));
    }

    // --- reserve interaction with LP withdraw -------------------------------

    function test_OpenPositionReservesAgainstLpWithdraw() public {
        _open(alice, true, 100_000 * ONE, 10); // big notional reserve

        uint256 reserved = perp.totalReserved();
        uint256 available = perp.availableAssets();
        assertEq(available, perp.totalAssets() - reserved);

        // LP tries to withdraw all shares -> assets exceed available -> revert.
        uint256 lpShares = perp.balanceOf(lp);
        uint256 lpAssets = perp.convertToAssets(lpShares);
        vm.prank(lp);
        vm.expectRevert(
            abi.encodeWithSelector(VolXPerp.WithdrawExceedsAvailable.selector, lpAssets, available)
        );
        perp.withdraw(lpShares);
    }
}
