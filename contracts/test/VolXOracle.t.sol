// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Test } from "forge-std/Test.sol";
import { VolXOracle } from "../src/VolXOracle.sol";
import { Ownable } from "@openzeppelin/contracts/access/Ownable.sol";

contract VolXOracleTest is Test {
    VolXOracle internal oracle;

    address internal owner = address(this);
    address internal keeper = makeAddr("keeper");
    address internal stranger = makeAddr("stranger");

    // Sample values at scale: BVOL 65.0 -> 65e8, conf 0.97 -> 970_000.
    uint64 internal constant BVOL_VAL = 65 * 1e8;
    uint32 internal constant BVOL_CONF = 970_000;
    uint64 internal constant EVOL_VAL = 80 * 1e8;
    uint32 internal constant EVOL_CONF = 985_000;

    event PriceUpdated(
        VolXOracle.Index indexed index, uint64 value, uint32 confidence, uint64 updatedAt
    );
    event KeeperUpdated(address indexed previousKeeper, address indexed newKeeper);

    function setUp() public {
        // Start at a non-zero timestamp so updatedAt==0 unambiguously means "never set".
        vm.warp(1_700_000_000);
        oracle = new VolXOracle(keeper);
    }

    // --- construction / config ---------------------------------------------

    function test_OwnerIsDeployer() public view {
        assertEq(oracle.owner(), owner);
    }

    function test_InitialKeeperSet() public view {
        assertEq(oracle.keeper(), keeper);
    }

    function test_ConstructorRejectsZeroKeeper() public {
        vm.expectRevert(VolXOracle.ZeroAddress.selector);
        new VolXOracle(address(0));
    }

    function test_Scales() public view {
        assertEq(oracle.VALUE_SCALE(), 1e8);
        assertEq(oracle.CONFIDENCE_SCALE(), 1e6);
        assertEq(oracle.MAX_STALENESS(), 1 hours);
    }

    // --- keeper rotation ----------------------------------------------------

    function test_OwnerCanSetKeeper() public {
        vm.expectEmit(true, true, false, false);
        emit KeeperUpdated(keeper, stranger);
        oracle.setKeeper(stranger);
        assertEq(oracle.keeper(), stranger);
    }

    function test_NonOwnerCannotSetKeeper() public {
        vm.prank(stranger);
        vm.expectRevert(
            abi.encodeWithSelector(Ownable.OwnableUnauthorizedAccount.selector, stranger)
        );
        oracle.setKeeper(stranger);
    }

    function test_SetKeeperRejectsZero() public {
        vm.expectRevert(VolXOracle.ZeroAddress.selector);
        oracle.setKeeper(address(0));
    }

    function test_RotatedKeeperCanUpdateOldCannot() public {
        oracle.setKeeper(stranger);

        vm.prank(keeper);
        vm.expectRevert(abi.encodeWithSelector(VolXOracle.NotKeeper.selector, keeper));
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, EVOL_VAL, EVOL_CONF);

        vm.prank(stranger);
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, EVOL_VAL, EVOL_CONF);
        (uint64 v,,) = oracle.getPrice(VolXOracle.Index.BVOL);
        assertEq(v, BVOL_VAL);
    }

    // --- authorisation on update -------------------------------------------

    function test_NonKeeperCannotUpdate() public {
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(VolXOracle.NotKeeper.selector, stranger));
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, EVOL_VAL, EVOL_CONF);
    }

    function test_OwnerIsNotKeeperByDefault() public {
        // Deployer (owner) is not the keeper here, so it cannot push.
        vm.expectRevert(abi.encodeWithSelector(VolXOracle.NotKeeper.selector, owner));
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, EVOL_VAL, EVOL_CONF);
    }

    // --- batch write --------------------------------------------------------

    function test_UpdateBothWritesBothIndices() public {
        vm.prank(keeper);
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, EVOL_VAL, EVOL_CONF);

        (uint64 bv, uint64 bt, uint32 bc) = oracle.getPrice(VolXOracle.Index.BVOL);
        (uint64 ev, uint64 et, uint32 ec) = oracle.getPrice(VolXOracle.Index.EVOL);

        assertEq(bv, BVOL_VAL);
        assertEq(bc, BVOL_CONF);
        assertEq(bt, uint64(block.timestamp));
        assertEq(ev, EVOL_VAL);
        assertEq(ec, EVOL_CONF);
        assertEq(et, uint64(block.timestamp));
    }

    function test_UpdateEmitsBothEvents() public {
        vm.expectEmit(true, false, false, true);
        emit PriceUpdated(VolXOracle.Index.BVOL, BVOL_VAL, BVOL_CONF, uint64(block.timestamp));
        vm.expectEmit(true, false, false, true);
        emit PriceUpdated(VolXOracle.Index.EVOL, EVOL_VAL, EVOL_CONF, uint64(block.timestamp));

        vm.prank(keeper);
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, EVOL_VAL, EVOL_CONF);
    }

    function test_SecondUpdateOverwrites() public {
        vm.prank(keeper);
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, EVOL_VAL, EVOL_CONF);

        vm.warp(block.timestamp + 120);
        vm.prank(keeper);
        oracle.updateBoth(70 * 1e8, 900_000, EVOL_VAL, EVOL_CONF);

        (uint64 bv, uint64 bt, uint32 bc) = oracle.getPrice(VolXOracle.Index.BVOL);
        assertEq(bv, 70 * 1e8);
        assertEq(bc, 900_000);
        assertEq(bt, uint64(block.timestamp));
    }

    // --- single-index update ------------------------------------------------

    function test_UpdateOneWritesOnlyTarget() public {
        vm.prank(keeper);
        oracle.updateOne(VolXOracle.Index.EVOL, EVOL_VAL, EVOL_CONF);

        (uint64 ev,, uint32 ec) = oracle.getPrice(VolXOracle.Index.EVOL);
        assertEq(ev, EVOL_VAL);
        assertEq(ec, EVOL_CONF);

        // BVOL untouched.
        (uint64 bv, uint64 bt,) = oracle.getPrice(VolXOracle.Index.BVOL);
        assertEq(bv, 0);
        assertEq(bt, 0);
    }

    function test_UpdateOneNonKeeperReverts() public {
        vm.prank(stranger);
        vm.expectRevert(abi.encodeWithSelector(VolXOracle.NotKeeper.selector, stranger));
        oracle.updateOne(VolXOracle.Index.BVOL, BVOL_VAL, BVOL_CONF);
    }

    function test_UpdateOneValidatesValue() public {
        vm.prank(keeper);
        vm.expectRevert(
            abi.encodeWithSelector(VolXOracle.ZeroValue.selector, VolXOracle.Index.BVOL)
        );
        oracle.updateOne(VolXOracle.Index.BVOL, 0, BVOL_CONF);
    }

    // --- validation ---------------------------------------------------------

    function test_ZeroValueReverts() public {
        vm.prank(keeper);
        vm.expectRevert(
            abi.encodeWithSelector(VolXOracle.ZeroValue.selector, VolXOracle.Index.BVOL)
        );
        oracle.updateBoth(0, BVOL_CONF, EVOL_VAL, EVOL_CONF);
    }

    function test_ZeroEvolValueRevertsAndRollsBackBvol() public {
        vm.prank(keeper);
        vm.expectRevert(
            abi.encodeWithSelector(VolXOracle.ZeroValue.selector, VolXOracle.Index.EVOL)
        );
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, 0, EVOL_CONF);

        // BVOL write in the same tx must be rolled back (atomicity).
        (uint64 v,,) = oracle.getPrice(VolXOracle.Index.BVOL);
        assertEq(v, 0);
    }

    function test_ConfidenceAboveScaleReverts() public {
        uint32 tooHigh = 1_000_001;
        vm.prank(keeper);
        vm.expectRevert(
            abi.encodeWithSelector(VolXOracle.ConfidenceTooHigh.selector, tooHigh, uint32(1e6))
        );
        oracle.updateBoth(BVOL_VAL, tooHigh, EVOL_VAL, EVOL_CONF);
    }

    function test_ConfidenceAtScaleAllowed() public {
        vm.prank(keeper);
        oracle.updateBoth(BVOL_VAL, 1_000_000, EVOL_VAL, 0);
        (,, uint32 bc) = oracle.getPrice(VolXOracle.Index.BVOL);
        (,, uint32 ec) = oracle.getPrice(VolXOracle.Index.EVOL);
        assertEq(bc, 1_000_000);
        assertEq(ec, 0);
    }

    // --- staleness ----------------------------------------------------------

    function test_GetPriceCheckedFreshPasses() public {
        vm.prank(keeper);
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, EVOL_VAL, EVOL_CONF);

        vm.warp(block.timestamp + oracle.MAX_STALENESS()); // exactly at edge: still fresh
        (uint64 v,,) = oracle.getPriceChecked(VolXOracle.Index.BVOL);
        assertEq(v, BVOL_VAL);
    }

    function test_GetPriceCheckedStaleReverts() public {
        vm.prank(keeper);
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, EVOL_VAL, EVOL_CONF);
        uint64 setAt = uint64(block.timestamp);

        vm.warp(block.timestamp + oracle.MAX_STALENESS() + 1); // one second past edge
        vm.expectRevert(
            abi.encodeWithSelector(
                VolXOracle.StalePrice.selector,
                VolXOracle.Index.BVOL,
                setAt,
                block.timestamp,
                oracle.MAX_STALENESS()
            )
        );
        oracle.getPriceChecked(VolXOracle.Index.BVOL);
    }

    function test_GetPriceCheckedNeverSetReverts() public {
        vm.expectRevert(
            abi.encodeWithSelector(VolXOracle.PriceNeverSet.selector, VolXOracle.Index.EVOL)
        );
        oracle.getPriceChecked(VolXOracle.Index.EVOL);
    }

    function test_GetPriceRawDoesNotCheckStaleness() public {
        vm.prank(keeper);
        oracle.updateBoth(BVOL_VAL, BVOL_CONF, EVOL_VAL, EVOL_CONF);

        vm.warp(block.timestamp + 30 days); // very stale
        (uint64 v,,) = oracle.getPrice(VolXOracle.Index.BVOL); // raw read still works
        assertEq(v, BVOL_VAL);
    }

    function test_GetPriceUnsetReturnsZeros() public view {
        (uint64 v, uint64 t, uint32 c) = oracle.getPrice(VolXOracle.Index.BVOL);
        assertEq(v, 0);
        assertEq(t, 0);
        assertEq(c, 0);
    }
}
