// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Test } from "forge-std/Test.sol";
import { MockUSDC } from "../src/MockUSDC.sol";
import { VolXOracle } from "../src/VolXOracle.sol";
import { VolXPerp } from "../src/VolXPerp.sol";

/// @notice Tests for liquidation + fee accounting (#90). Test contract is the
/// oracle keeper; `liquidator` calls liquidate().
contract VolXPerpLiquidationTest is Test {
    MockUSDC internal usdc;
    VolXOracle internal oracle;
    VolXPerp internal perp;

    address internal lp = makeAddr("lp");
    address internal alice = makeAddr("alice");
    address internal liquidator = makeAddr("liquidator");

    uint256 internal constant ONE = 1e6;
    uint64 internal constant ENTRY = 60e8;
    uint32 internal constant CONF = 1e6;

    event PositionLiquidated(
        uint256 indexed id,
        address indexed trader,
        address indexed liquidator,
        uint256 markPrice,
        uint256 reward,
        uint256 vaultGain
    );

    function setUp() public {
        vm.warp(1_700_000_000);
        usdc = new MockUSDC();
        oracle = new VolXOracle(address(this));
        perp = new VolXPerp(usdc, oracle);

        _fund(lp, 2_000_000 * ONE);
        _fund(alice, 100_000 * ONE);

        vm.prank(lp);
        perp.deposit(1_000_000 * ONE);
        oracle.updateOne(VolXOracle.Index.BVOL, ENTRY, CONF);
    }

    function _fund(address who, uint256 amount) internal {
        deal(address(usdc), who, amount);
        vm.prank(who);
        usdc.approve(address(perp), type(uint256).max);
    }

    function _setBvol(uint64 v) internal {
        oracle.updateOne(VolXOracle.Index.BVOL, v, CONF);
    }

    // Open a 10x long: collateral 1000 -> working 990, notional 9900.
    function _openLong10x() internal returns (uint256 id) {
        vm.prank(alice);
        id = perp.openPosition(VolXOracle.Index.BVOL, true, 1000 * ONE, 10);
    }

    // --- threshold ----------------------------------------------------------

    function test_HealthyPositionNotLiquidatable() public {
        uint256 id = _openLong10x();
        _setBvol(558e7); // -7% -> loss 693 < threshold 792

        assertFalse(perp.isLiquidatable(id));
        vm.prank(liquidator);
        vm.expectRevert(
            abi.encodeWithSelector(VolXPerp.NotLiquidatable.selector, id, 693 * ONE, 792 * ONE)
        );
        perp.liquidate(id);
    }

    function test_WinningPositionNotLiquidatable() public {
        uint256 id = _openLong10x();
        _setBvol(66e8); // +10% -> profit, loss 0

        assertFalse(perp.isLiquidatable(id));
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(VolXPerp.NotLiquidatable.selector, id, 0, 792 * ONE));
        perp.liquidate(id);
    }

    function test_LiquidatableAtThreshold() public {
        uint256 id = _openLong10x();
        _setBvol(552e7); // -8% -> loss 792 == threshold
        assertTrue(perp.isLiquidatable(id));
    }

    // --- liquidate ----------------------------------------------------------

    function test_LiquidateRewardsCallerAndVault() public {
        uint256 id = _openLong10x();
        uint256 taAfterOpen = perp.totalAssets();
        _setBvol(552e7); // -8%

        // equity = 990 - 792 = 198; reward = min(1% * 990 = 9.9, 198) = 9.9.
        uint256 expectedReward = 99 * ONE / 10; // 9.9
        uint256 expectedVaultGain = (990 * ONE) - expectedReward;

        vm.prank(liquidator);
        perp.liquidate(id);

        assertEq(usdc.balanceOf(liquidator), expectedReward);
        assertEq(perp.totalAssets() - taAfterOpen, expectedVaultGain);
        assertEq(perp.totalReserved(), 0);
        (address trader,,,,,,) = perp.positions(id);
        assertEq(trader, address(0));
    }

    function test_LiquidateEmitsEvent() public {
        uint256 id = _openLong10x();
        _setBvol(552e7); // -8%
        uint256 expectedReward = 99 * ONE / 10;
        uint256 expectedVaultGain = (990 * ONE) - expectedReward;

        vm.expectEmit(true, true, true, true);
        emit PositionLiquidated(id, alice, liquidator, 552e7, expectedReward, expectedVaultGain);
        vm.prank(liquidator);
        perp.liquidate(id);
    }

    function test_DeepUnderwaterRewardIsZero() public {
        uint256 id = _openLong10x();
        _setBvol(51e8); // -15% -> loss 1485 > collateral; equity 0

        assertTrue(perp.isLiquidatable(id));
        uint256 taAfterOpen = perp.totalAssets();

        vm.prank(liquidator);
        perp.liquidate(id);

        assertEq(usdc.balanceOf(liquidator), 0); // nothing left to reward
        assertEq(perp.totalAssets() - taAfterOpen, 990 * ONE); // vault takes all
    }

    function test_LiquidateRevertsOnStaleOracle() public {
        uint256 id = _openLong10x();
        _setBvol(552e7); // -8%, would be liquidatable when fresh

        uint256 maxStale = oracle.MAX_STALENESS();
        vm.warp(block.timestamp + maxStale + 1);
        bytes memory err = abi.encodeWithSelector(
            VolXOracle.StalePrice.selector,
            VolXOracle.Index.BVOL,
            uint64(1_700_000_000), // price was last set at setUp's warp time
            block.timestamp,
            maxStale
        );

        vm.prank(liquidator);
        vm.expectRevert(err);
        perp.liquidate(id);
    }

    function test_LiquidateNonexistentReverts() public {
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(VolXPerp.PositionNotFound.selector, 42));
        perp.liquidate(42);
    }

    function test_NoDoubleCloseAfterLiquidate() public {
        uint256 id = _openLong10x();
        _setBvol(552e7);
        vm.prank(liquidator);
        perp.liquidate(id);

        // Re-liquidate and owner close both revert: position is gone.
        vm.prank(liquidator);
        vm.expectRevert(abi.encodeWithSelector(VolXPerp.PositionNotFound.selector, id));
        perp.liquidate(id);

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(VolXPerp.PositionNotFound.selector, id));
        perp.closePosition(id);
    }

    function test_IsLiquidatableNonexistentIsFalse() public view {
        assertFalse(perp.isLiquidatable(42));
    }
}
