// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Test } from "forge-std/Test.sol";
import { MockUSDC } from "../src/MockUSDC.sol";
import { VolXPerp } from "../src/VolXPerp.sol";
import { VolXPerpHarness } from "./harness/VolXPerpHarness.sol";
import { ReentrantToken } from "./mocks/ReentrantToken.sol";
import { VolXOracle } from "../src/VolXOracle.sol";
import { ReentrancyGuard } from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";

contract VolXPerpTest is Test {
    MockUSDC internal usdc;
    VolXOracle internal oracle;
    VolXPerpHarness internal vault;

    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");

    uint256 internal constant ONE = 1e6; // 1 mUSDC (6 decimals)

    event Deposit(address indexed caller, uint256 assets, uint256 shares);
    event Withdraw(address indexed caller, uint256 assets, uint256 shares);

    function setUp() public {
        usdc = new MockUSDC();
        oracle = new VolXOracle(address(this));
        vault = new VolXPerpHarness(usdc, oracle);
        _fund(alice, 10_000 * ONE);
        _fund(bob, 10_000 * ONE);
    }

    function _fund(address who, uint256 amount) internal {
        deal(address(usdc), who, amount); // bypasses the 10k faucet cap
        vm.prank(who);
        usdc.approve(address(vault), type(uint256).max);
    }

    /// @dev Simulate a trader loss / fee: bump internal accounting AND back it
    /// with real collateral (in #89 the trader's lost margin actually moves in),
    /// so the vault stays solvent for withdrawals.
    function _credit(uint256 amount) internal {
        deal(address(usdc), address(vault), usdc.balanceOf(address(vault)) + amount);
        vault.creditVault(amount);
    }

    // --- metadata -----------------------------------------------------------

    function test_SharesDecimalsMatchAsset() public view {
        assertEq(vault.decimals(), 6);
        assertEq(address(vault.asset()), address(usdc));
    }

    function test_ConstructorRejectsZeroAsset() public {
        vm.expectRevert(VolXPerp.ZeroAddress.selector);
        new VolXPerpHarness(MockUSDC(address(0)), oracle);
    }

    // --- deposit ------------------------------------------------------------

    function test_FirstDepositMintsOneToOne() public {
        vm.prank(alice);
        uint256 shares = vault.deposit(1000 * ONE);

        assertEq(shares, 1000 * ONE);
        assertEq(vault.balanceOf(alice), 1000 * ONE);
        assertEq(vault.totalAssets(), 1000 * ONE);
        assertEq(vault.totalSupply(), 1000 * ONE);
        assertEq(usdc.balanceOf(address(vault)), 1000 * ONE);
    }

    function test_DepositZeroReverts() public {
        vm.prank(alice);
        vm.expectRevert(VolXPerp.ZeroAssets.selector);
        vault.deposit(0);
    }

    function test_DepositEmits() public {
        vm.expectEmit(true, false, false, true);
        emit Deposit(alice, 1000 * ONE, 1000 * ONE);
        vm.prank(alice);
        vault.deposit(1000 * ONE);
    }

    function test_SecondDepositRespectsExchangeRate() public {
        // Alice seeds 1000, then a trader loss credits 500 -> rate 1.5 assets/share.
        vm.prank(alice);
        vault.deposit(1000 * ONE);
        _credit(500 * ONE);
        assertEq(vault.totalAssets(), 1500 * ONE);
        assertEq(vault.totalSupply(), 1000 * ONE);

        // Bob deposits 1500 -> 1500 * 1000 / 1500 = 1000 shares.
        vm.prank(bob);
        uint256 bobShares = vault.deposit(1500 * ONE);
        assertEq(bobShares, 1000 * ONE);
        assertEq(vault.totalAssets(), 3000 * ONE);
        assertEq(vault.totalSupply(), 2000 * ONE);
    }

    // --- withdraw -----------------------------------------------------------

    function test_WithdrawReturnsProportionalAssets() public {
        vm.prank(alice);
        vault.deposit(1000 * ONE);
        _credit(500 * ONE); // rate 1.5

        uint256 before = usdc.balanceOf(alice);
        vm.prank(alice);
        uint256 assets = vault.withdraw(1000 * ONE);

        assertEq(assets, 1500 * ONE); // 1000 shares * 1500/1000
        assertEq(usdc.balanceOf(alice) - before, 1500 * ONE);
        assertEq(vault.totalSupply(), 0);
        assertEq(vault.totalAssets(), 0);
    }

    function test_WithdrawZeroReverts() public {
        vm.prank(alice);
        vm.expectRevert(VolXPerp.ZeroShares.selector);
        vault.withdraw(0);
    }

    function test_WithdrawMoreSharesThanOwnedReverts() public {
        vm.prank(alice);
        vault.deposit(1000 * ONE);

        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(VolXPerp.InsufficientShares.selector, 1000 * ONE, 1001 * ONE)
        );
        vault.withdraw(1001 * ONE);
    }

    function test_WithdrawEmits() public {
        vm.prank(alice);
        vault.deposit(1000 * ONE);

        vm.expectEmit(true, false, false, true);
        emit Withdraw(alice, 1000 * ONE, 1000 * ONE);
        vm.prank(alice);
        vault.withdraw(1000 * ONE);
    }

    function test_DebitVaultReducesRedeemValue() public {
        vm.prank(alice);
        vault.deposit(1000 * ONE);
        vault.debitVault(400 * ONE); // trader win paid out -> rate 0.6

        vm.prank(alice);
        uint256 assets = vault.withdraw(1000 * ONE);
        assertEq(assets, 600 * ONE);
    }

    function test_WithdrawRoundingToZeroReverts() public {
        // supply 3, _totalAssets 1 -> convertToAssets(1) floors to 0.
        vm.prank(alice);
        vault.deposit(3);
        vault.debitVault(2); // accounting now 1, supply 3

        vm.prank(alice);
        vm.expectRevert(VolXPerp.ZeroAssets.selector);
        vault.withdraw(1);
    }

    // --- reserve guard ------------------------------------------------------

    function test_CannotWithdrawReservedCollateral() public {
        vm.prank(alice);
        vault.deposit(1000 * ONE);
        vault.setReserved(600 * ONE); // available = 400
        assertEq(vault.availableAssets(), 400 * ONE);

        // 401 shares -> 401 assets > 400 available: revert.
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(VolXPerp.WithdrawExceedsAvailable.selector, 401 * ONE, 400 * ONE)
        );
        vault.withdraw(401 * ONE);

        // Exactly 400 is allowed.
        vm.prank(alice);
        uint256 assets = vault.withdraw(400 * ONE);
        assertEq(assets, 400 * ONE);
    }

    function test_FullReserveBlocksAllWithdraw() public {
        vm.prank(alice);
        vault.deposit(1000 * ONE);
        vault.setReserved(1000 * ONE);
        assertEq(vault.availableAssets(), 0);

        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(VolXPerp.WithdrawExceedsAvailable.selector, 1 * ONE, 0)
        );
        vault.withdraw(1 * ONE);
    }

    // --- donation resistance ------------------------------------------------

    function test_DirectDonationDoesNotInflateSharePrice() public {
        vm.prank(alice);
        vault.deposit(1000 * ONE);

        // Attacker donates 5000 mUSDC directly to the vault contract.
        usdc.mint(address(this), 5000 * ONE);
        assertTrue(usdc.transfer(address(vault), 5000 * ONE));

        // Internal accounting unchanged: rate stays 1:1.
        assertEq(vault.totalAssets(), 1000 * ONE);
        assertEq(vault.convertToAssets(1000 * ONE), 1000 * ONE);

        // Bob's deposit still mints 1:1, not diluted by the donation.
        vm.prank(bob);
        uint256 bobShares = vault.deposit(1000 * ONE);
        assertEq(bobShares, 1000 * ONE);
    }

    // --- reentrancy ---------------------------------------------------------

    function test_WithdrawIsReentrancyGuarded() public {
        ReentrantToken evil = new ReentrantToken();
        VolXPerpHarness evilVault = new VolXPerpHarness(evil, oracle);
        evil.setVault(address(evilVault));

        evil.mint(alice, 1000 * ONE);
        vm.startPrank(alice);
        evil.approve(address(evilVault), type(uint256).max);
        evilVault.deposit(1000 * ONE);
        vm.stopPrank();

        evil.armAttack(true);

        vm.prank(alice);
        vm.expectRevert(ReentrancyGuard.ReentrancyGuardReentrantCall.selector);
        evilVault.withdraw(1000 * ONE);
    }

    // --- invariants / fuzz --------------------------------------------------

    function testFuzz_SoloDepositWithdrawNeverGains(uint256 amount) public {
        amount = bound(amount, 1, 1_000_000 * ONE);
        deal(address(usdc), alice, amount);
        vm.startPrank(alice);
        usdc.approve(address(vault), type(uint256).max);
        uint256 shares = vault.deposit(amount);
        uint256 out = vault.withdraw(shares);
        vm.stopPrank();
        // Round-trip never returns more than was put in.
        assertLe(out, amount);
    }

    function testFuzz_RedeemNeverExceedsConvertedAssets(uint256 deposit, uint256 credit) public {
        deposit = bound(deposit, ONE, 1_000_000 * ONE);
        credit = bound(credit, 0, 1_000_000 * ONE);

        deal(address(usdc), alice, deposit);
        vm.startPrank(alice);
        usdc.approve(address(vault), type(uint256).max);
        vault.deposit(deposit);
        vm.stopPrank();
        vault.creditVault(credit); // accounting-only; no withdraw in this test

        uint256 shares = vault.balanceOf(alice);
        // convertToShares(convertToAssets(s)) <= s — floor rounding never favors LP.
        uint256 assets = vault.convertToAssets(shares);
        assertLe(vault.convertToShares(assets), shares);
    }
}
