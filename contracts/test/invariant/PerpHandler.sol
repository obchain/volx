// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Test } from "forge-std/Test.sol";
import { MockUSDC } from "../../src/MockUSDC.sol";
import { VolXOracle } from "../../src/VolXOracle.sol";
import { VolXPerp } from "../../src/VolXPerp.sol";
import { SafeCast } from "@openzeppelin/contracts/utils/math/SafeCast.sol";
import { Math } from "@openzeppelin/contracts/utils/math/Math.sol";

/// @notice Stateful fuzzing handler for VolXPerp invariants. The handler is the
/// oracle keeper, so it can move prices. Every action is wrapped in try/catch so
/// legitimate reverts (e.g. withdraw exceeding available) don't abort the run;
/// invariants are checked on whatever valid states are reached.
contract PerpHandler is Test {
    using SafeCast for uint256;

    MockUSDC public usdc;
    VolXOracle public oracle;
    VolXPerp public perp;

    address[] public actors;
    uint256[] public openedIds;

    constructor(MockUSDC usdc_, VolXOracle oracle_, VolXPerp perp_) {
        usdc = usdc_;
        oracle = oracle_;
        perp = perp_;
        actors.push(makeAddr("lp1"));
        actors.push(makeAddr("lp2"));
        actors.push(makeAddr("trader1"));
        actors.push(makeAddr("trader2"));
    }

    function openedIdsLength() external view returns (uint256) {
        return openedIds.length;
    }

    function actorsLength() external view returns (uint256) {
        return actors.length;
    }

    function _actor(uint256 seed) internal view returns (address) {
        return actors[seed % actors.length];
    }

    function _index(uint256 seed) internal pure returns (VolXOracle.Index) {
        return seed % 2 == 0 ? VolXOracle.Index.BVOL : VolXOracle.Index.EVOL;
    }

    /// @dev Share price scaled to 1e18; 0 when there is no supply.
    function _price() internal view returns (uint256) {
        uint256 s = perp.totalSupply();
        if (s == 0) return 0;
        return Math.mulDiv(perp.totalAssets(), 1e18, s);
    }

    /// @dev Asserts the share price did not fall across an action that only
    /// accrues fees / vault gains (everything except a winning close). Deposits
    /// and withdrawals round in the vault's favor, so price is non-decreasing.
    modifier nonDecreasingPrice() {
        uint256 before = _price();
        _;
        uint256 afterPrice = _price();
        if (before > 0 && afterPrice > 0) assertGe(afterPrice, before);
    }

    // --- LP actions ---------------------------------------------------------

    function deposit(uint256 actorSeed, uint256 amount) external nonDecreasingPrice {
        address a = _actor(actorSeed);
        amount = bound(amount, 1e6, 1_000_000e6);
        deal(address(usdc), a, amount);
        vm.startPrank(a);
        usdc.approve(address(perp), amount);
        try perp.deposit(amount) { } catch { }
        vm.stopPrank();
    }

    function withdraw(uint256 actorSeed, uint256 shareSeed) external nonDecreasingPrice {
        address a = _actor(actorSeed);
        uint256 bal = perp.balanceOf(a);
        if (bal == 0) return;
        uint256 shares = bound(shareSeed, 1, bal);
        vm.prank(a);
        try perp.withdraw(shares) { } catch { }
    }

    // --- trader actions -----------------------------------------------------

    function openPosition(
        uint256 actorSeed,
        uint256 idxSeed,
        bool isLong,
        uint256 coll,
        uint256 lev
    ) external nonDecreasingPrice {
        address a = _actor(actorSeed);
        coll = bound(coll, 2e6, 50_000e6);
        lev = bound(lev, 1, 10);
        VolXOracle.Index idx = _index(idxSeed);
        deal(address(usdc), a, coll);
        vm.startPrank(a);
        usdc.approve(address(perp), coll);
        try perp.openPosition(idx, isLong, coll, lev) returns (uint256 id) {
            openedIds.push(id);
        } catch { }
        vm.stopPrank();
    }

    function closePosition(uint256 idSeed) external {
        if (openedIds.length == 0) return;
        uint256 idx = idSeed % openedIds.length;
        uint256 id = openedIds[idx];
        (address trader,,,,,,) = perp.positions(id);
        if (trader == address(0)) return;

        // A winning close pays the trader from the vault and may lower the share
        // price; any other close (loss/breakeven) only accrues to the vault.
        (int256 pnl,) = perp.positionValue(id);
        uint256 before = _price();
        vm.prank(trader);
        try perp.closePosition(id) {
            _removeId(idx);
            uint256 afterPrice = _price();
            if (pnl <= 0 && before > 0 && afterPrice > 0) assertGe(afterPrice, before);
        } catch { }
    }

    function liquidate(uint256 idSeed, uint256 actorSeed) external nonDecreasingPrice {
        if (openedIds.length == 0) return;
        uint256 idx = idSeed % openedIds.length;
        uint256 id = openedIds[idx];
        (address trader,,,,,,) = perp.positions(id);
        if (trader == address(0)) return;
        vm.prank(_actor(actorSeed));
        try perp.liquidate(id) {
            _removeId(idx);
        } catch { }
    }

    /// @dev Swap-and-pop to keep `openedIds` dense with live positions, so the
    /// fuzzer keeps hitting real close/liquidate transitions at higher depth.
    function _removeId(uint256 idx) internal {
        openedIds[idx] = openedIds[openedIds.length - 1];
        openedIds.pop();
    }

    // --- oracle (keeper) ----------------------------------------------------

    function movePrice(uint256 idxSeed, uint256 price) external nonDecreasingPrice {
        price = bound(price, 1e8, 500e8); // 1.0 .. 500.0 at 1e8 scale
        try oracle.updateOne(_index(idxSeed), price.toUint64(), 1e6) { } catch { }
    }
}
