// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Test } from "forge-std/Test.sol";
import { MockUSDC } from "../src/MockUSDC.sol";
import { VolXOracle } from "../src/VolXOracle.sol";
import { VolXPerpV2 } from "../src/VolXPerpV2.sol";

/// @notice Tests for the v2 additions: borrow-fee funding + conditional orders.
/// The test contract is the oracle keeper, so it pushes prices directly.
contract VolXPerpV2Test is Test {
    MockUSDC internal usdc;
    VolXOracle internal oracle;
    VolXPerpV2 internal perp;

    address internal lp = makeAddr("lp");
    address internal alice = makeAddr("alice");
    address internal keeper = makeAddr("keeper");

    uint256 internal constant ONE = 1e6;
    uint64 internal constant ENTRY = 60e8; // BVOL 60.0 @1e8
    uint32 internal constant CONF = 1e6;
    VolXOracle.Index internal constant BVOL = VolXOracle.Index.BVOL;

    function setUp() public {
        vm.warp(1_700_000_000);
        usdc = new MockUSDC();
        oracle = new VolXOracle(address(this));
        perp = new VolXPerpV2(usdc, oracle);

        _fund(lp, 2_000_000 * ONE);
        _fund(alice, 200_000 * ONE);
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
        oracle.updateOne(BVOL, v, CONF);
    }

    function _open(address who, bool isLong, uint256 collateral, uint256 leverage) internal returns (uint256 id) {
        vm.prank(who);
        id = perp.openPosition(BVOL, isLong, collateral, leverage);
    }

    // --- funding ------------------------------------------------------------

    function test_FundingZeroAtOpenThenGrows() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        assertEq(perp.accruedFunding(id), 0);
        vm.warp(block.timestamp + 1 days);
        // notional = working(995) * 5 = 4975; 30 bps/day => 4975 * 0.003 = 14.925
        assertEq(perp.accruedFunding(id), 14_925_000);
    }

    function test_SetFundingRateOwnerOnlyAndCapped() public {
        vm.prank(alice);
        vm.expectRevert();
        perp.setFundingRate(50);

        perp.setFundingRate(50);
        assertEq(perp.fundingBpsPerDay(), 50);

        vm.expectRevert(abi.encodeWithSelector(VolXPerpV2.FundingRateTooHigh.selector, 1001, 1000));
        perp.setFundingRate(1001);
    }

    function test_CloseChargesFundingToVault() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        uint256 taAfterOpen = perp.totalAssets();
        vm.warp(block.timestamp + 10 days);
        _setBvol(ENTRY); // refresh oracle timestamp (price unchanged) so close isn't stale
        uint256 funding = perp.accruedFunding(id); // 149.25
        uint256 balBefore = usdc.balanceOf(alice);

        vm.prank(alice);
        perp.closePosition(id);

        // Flat price => pnl 0. payout = 995 - funding - closeFee.
        uint256 closeFee = (995 * ONE * 5 * 10) / 10_000; // notional*0.1%
        uint256 expectedPayout = 995 * ONE - funding - closeFee;
        assertEq(usdc.balanceOf(alice) - balBefore, expectedPayout);
        // Vault gained funding + closeFee over the post-open baseline.
        assertEq(perp.totalAssets(), taAfterOpen + funding + closeFee);
    }

    function test_FundingCanMakePositionLiquidatable() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        assertFalse(perp.isLiquidatable(id));
        // Flat price, but borrow fee eats >80% of collateral over time.
        vm.warp(block.timestamp + 60 days);
        assertTrue(perp.isLiquidatable(id));

        // Anyone can liquidate; vault keeps collateral minus the 1% reward.
        _setBvol(ENTRY); // refresh oracle (liquidate uses the staleness-checked read)
        vm.prank(keeper);
        perp.liquidate(id);
        (address trader,,,,,,) = perp.positions(id);
        assertEq(trader, address(0));
    }

    function test_PositionValueNetsFunding() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        vm.warp(block.timestamp + 5 days);
        (int256 pnl, uint256 equity) = perp.positionValue(id);
        assertEq(pnl, 0); // flat price
        assertEq(equity, 995 * ONE - perp.accruedFunding(id));
    }

    // --- limit-open orders --------------------------------------------------

    function test_PlaceLimitOpenEscrowsCollateral() public {
        uint256 taBefore = perp.totalAssets();
        uint256 balBefore = usdc.balanceOf(alice);
        vm.prank(alice);
        uint256 oid = perp.placeLimitOpen(BVOL, true, 1000 * ONE, 5, 55e8, false);
        // Collateral left the trader but did NOT enter vault accounting.
        assertEq(balBefore - usdc.balanceOf(alice), 1000 * ONE);
        assertEq(perp.totalAssets(), taBefore);
        (address t,,,,,,,,) = perp.orders(oid);
        assertEq(t, alice);
    }

    function test_LimitOpenExecutesWhenTriggered() public {
        // Limit long: open when price drops to <= 55.
        vm.prank(alice);
        uint256 oid = perp.placeLimitOpen(BVOL, true, 1000 * ONE, 5, 55e8, false);

        // Not triggered at 60.
        vm.expectRevert();
        perp.executeOrder(oid);

        _setBvol(55e8);
        vm.prank(keeper);
        uint256 pid = perp.executeOrder(oid);

        (address trader,, bool isLong, uint256 collateral,, uint256 entry,) = perp.positions(pid);
        assertEq(trader, alice);
        assertTrue(isLong);
        assertEq(entry, 55e8);
        assertEq(collateral, 995 * ONE); // 1000 - openFee
        // Order consumed.
        (address ot,,,,,,,,) = perp.orders(oid);
        assertEq(ot, address(0));
    }

    function test_LimitOpenTriggerAbove() public {
        vm.prank(alice);
        uint256 oid = perp.placeLimitOpen(BVOL, false, 1000 * ONE, 3, 65e8, true);
        vm.expectRevert();
        perp.executeOrder(oid);
        _setBvol(65e8);
        vm.prank(keeper);
        uint256 pid = perp.executeOrder(oid);
        (address trader,, bool isLong,,,,) = perp.positions(pid);
        assertEq(trader, alice);
        assertFalse(isLong);
    }

    function test_CancelLimitOpenRefunds() public {
        uint256 balBefore = usdc.balanceOf(alice);
        vm.prank(alice);
        uint256 oid = perp.placeLimitOpen(BVOL, true, 1000 * ONE, 5, 55e8, false);
        assertEq(balBefore - usdc.balanceOf(alice), 1000 * ONE);
        vm.prank(alice);
        perp.cancelOrder(oid);
        assertEq(usdc.balanceOf(alice), balBefore); // fully refunded
    }

    function test_CancelOrderNotOwnerReverts() public {
        vm.prank(alice);
        uint256 oid = perp.placeLimitOpen(BVOL, true, 1000 * ONE, 5, 55e8, false);
        vm.prank(keeper);
        vm.expectRevert(abi.encodeWithSelector(VolXPerpV2.NotOrderOwner.selector, keeper, alice));
        perp.cancelOrder(oid);
    }

    // --- take-profit / stop-loss -------------------------------------------

    function test_PlaceStopRequiresOwnership() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        vm.prank(keeper);
        vm.expectRevert(abi.encodeWithSelector(VolXPerpV2.NotPositionOwner.selector, keeper, alice));
        perp.placeStop(id, 70e8, true, true);
    }

    function test_TakeProfitClosesOnTrigger() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5); // long @60
        vm.prank(alice);
        uint256 oid = perp.placeStop(id, 66e8, true, true); // TP at >= 66

        vm.expectRevert();
        perp.executeOrder(oid); // not yet

        _setBvol(66e8); // +10% => long profit
        uint256 balBefore = usdc.balanceOf(alice);
        vm.prank(keeper);
        perp.executeOrder(oid);

        // Position closed, trader paid a profit (> working collateral).
        (address trader,,,,,,) = perp.positions(id);
        assertEq(trader, address(0));
        assertGt(usdc.balanceOf(alice) - balBefore, 995 * ONE);
    }

    function test_StopLossClosesOnTrigger() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5); // long @60
        vm.prank(alice);
        uint256 oid = perp.placeStop(id, 57e8, false, false); // SL at <= 57

        _setBvol(57e8); // -5% * 5x = -25% loss
        vm.prank(keeper);
        perp.executeOrder(oid);
        (address trader,,,,,,) = perp.positions(id);
        assertEq(trader, address(0));
    }

    function test_ExecuteStopOnGonePositionClearsGracefully() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        vm.prank(alice);
        uint256 oid = perp.placeStop(id, 66e8, true, true);
        // Trader closes manually first.
        vm.prank(alice);
        perp.closePosition(id);

        // Order still armed; trigger it — should just clear, not revert.
        _setBvol(66e8);
        vm.prank(keeper);
        perp.executeOrder(oid);
        (address ot,,,,,,,,) = perp.orders(oid);
        assertEq(ot, address(0));
    }

    function test_ExecuteUnknownOrderReverts() public {
        vm.expectRevert(abi.encodeWithSelector(VolXPerpV2.OrderNotFound.selector, uint256(99)));
        perp.executeOrder(99);
    }

    // --- base sanity --------------------------------------------------------

    function test_OpenCloseRoundTripNoFundingAtSameBlock() public {
        uint256 id = _open(alice, true, 1000 * ONE, 5);
        uint256 balBefore = usdc.balanceOf(alice);
        // Same timestamp => funding 0; flat price => pnl 0; only close fee.
        vm.prank(alice);
        perp.closePosition(id);
        uint256 closeFee = (995 * ONE * 5 * 10) / 10_000;
        assertEq(usdc.balanceOf(alice) - balBefore, 995 * ONE - closeFee);
    }
}
