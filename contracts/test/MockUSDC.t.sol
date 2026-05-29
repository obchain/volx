// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Test } from "forge-std/Test.sol";
import { MockUSDC } from "../src/MockUSDC.sol";
import { IERC20Errors } from "@openzeppelin/contracts/interfaces/draft-IERC6093.sol";

contract MockUSDCTest is Test {
    MockUSDC internal usdc;

    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");

    function setUp() public {
        usdc = new MockUSDC();
    }

    // --- metadata -----------------------------------------------------------

    function test_DecimalsIsSix() public view {
        assertEq(usdc.decimals(), 6);
    }

    function test_NameAndSymbol() public view {
        assertEq(usdc.name(), "Mock USDC");
        assertEq(usdc.symbol(), "mUSDC");
    }

    function test_CapAndFaucetConstants() public view {
        assertEq(usdc.MAX_MINT_PER_CALL(), 10_000 * 1e6);
        assertEq(usdc.FAUCET_AMOUNT(), 10_000 * 1e6);
    }

    // --- faucet mint --------------------------------------------------------

    function test_AnyoneCanMintUpToCap() public {
        vm.prank(alice);
        usdc.mint(alice, usdc.MAX_MINT_PER_CALL());
        assertEq(usdc.balanceOf(alice), usdc.MAX_MINT_PER_CALL());
        assertEq(usdc.totalSupply(), usdc.MAX_MINT_PER_CALL());
    }

    function test_MintBelowCapSucceeds() public {
        vm.prank(bob);
        usdc.mint(bob, 1000 * 1e6);
        assertEq(usdc.balanceOf(bob), 1000 * 1e6);
    }

    function test_MintToOtherRecipient() public {
        vm.prank(alice);
        usdc.mint(bob, 500 * 1e6);
        assertEq(usdc.balanceOf(bob), 500 * 1e6);
        assertEq(usdc.balanceOf(alice), 0);
    }

    function test_OverCapMintReverts() public {
        uint256 cap = usdc.MAX_MINT_PER_CALL();
        vm.expectRevert(
            abi.encodeWithSelector(MockUSDC.MintAmountExceedsCap.selector, cap + 1, cap)
        );
        usdc.mint(alice, cap + 1);
    }

    function test_FaucetMintsFixedAmountToCaller() public {
        vm.prank(alice);
        usdc.faucet();
        assertEq(usdc.balanceOf(alice), usdc.FAUCET_AMOUNT());
    }

    function test_FaucetIsRepeatable() public {
        vm.startPrank(alice);
        usdc.faucet();
        usdc.faucet();
        vm.stopPrank();
        assertEq(usdc.balanceOf(alice), 2 * usdc.FAUCET_AMOUNT());
    }

    function testFuzz_MintRespectsCap(uint256 amount) public {
        if (amount > usdc.MAX_MINT_PER_CALL()) {
            vm.expectRevert(
                abi.encodeWithSelector(
                    MockUSDC.MintAmountExceedsCap.selector, amount, usdc.MAX_MINT_PER_CALL()
                )
            );
            usdc.mint(alice, amount);
        } else {
            usdc.mint(alice, amount);
            assertEq(usdc.balanceOf(alice), amount);
        }
    }

    // --- standard ERC20 -----------------------------------------------------

    function test_Transfer() public {
        vm.prank(alice);
        usdc.mint(alice, 1000 * 1e6);

        vm.prank(alice);
        bool ok = usdc.transfer(bob, 400 * 1e6);

        assertTrue(ok);
        assertEq(usdc.balanceOf(alice), 600 * 1e6);
        assertEq(usdc.balanceOf(bob), 400 * 1e6);
    }

    function test_ApproveAndTransferFrom() public {
        vm.prank(alice);
        usdc.mint(alice, 1000 * 1e6);

        vm.prank(alice);
        usdc.approve(bob, 300 * 1e6);
        assertEq(usdc.allowance(alice, bob), 300 * 1e6);

        vm.prank(bob);
        assertTrue(usdc.transferFrom(alice, bob, 300 * 1e6));

        assertEq(usdc.balanceOf(bob), 300 * 1e6);
        assertEq(usdc.balanceOf(alice), 700 * 1e6);
        assertEq(usdc.allowance(alice, bob), 0);
    }

    function test_TransferFromWithoutAllowanceReverts() public {
        vm.prank(alice);
        usdc.mint(alice, 1000 * 1e6);

        vm.prank(bob);
        vm.expectRevert(
            abi.encodeWithSelector(
                IERC20Errors.ERC20InsufficientAllowance.selector, bob, 0, 100 * 1e6
            )
        );
        // forge-lint: disable-next-line(erc20-unchecked-transfer)
        usdc.transferFrom(alice, bob, 100 * 1e6);
    }

    function test_TransferInsufficientBalanceReverts() public {
        vm.prank(alice);
        vm.expectRevert(
            abi.encodeWithSelector(IERC20Errors.ERC20InsufficientBalance.selector, alice, 0, 1)
        );
        // forge-lint: disable-next-line(erc20-unchecked-transfer)
        usdc.transfer(bob, 1);
    }
}
