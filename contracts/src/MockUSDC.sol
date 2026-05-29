// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { ERC20 } from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// @title MockUSDC
/// @notice 6-decimal test collateral for the VolX perp demo on Sepolia. Real
/// USDC is not freely mintable on testnet, so this ships a public faucet that
/// lets demo users self-fund. Testnet only — no value, no bridging, not audited.
/// @dev Minting is permissionless but per-call capped so nobody fat-fingers a
/// quadrillion-token balance that breaks UI formatting. There is no owner and
/// no supply ceiling: this is throwaway test money.
contract MockUSDC is ERC20 {
    /// @notice USDC convention: 6 decimals (not the ERC20 default of 18).
    uint8 private constant DECIMALS = 6;

    /// @notice Max tokens mintable in a single `mint` call: 10,000 mUSDC.
    /// @dev Expressed in base units (10_000 * 10**6).
    uint256 public constant MAX_MINT_PER_CALL = 10_000 * 10 ** uint256(DECIMALS);

    /// @notice Amount the convenience `faucet()` mints to the caller: 10,000 mUSDC.
    uint256 public constant FAUCET_AMOUNT = 10_000 * 10 ** uint256(DECIMALS);

    /// @notice Thrown when a `mint` exceeds {MAX_MINT_PER_CALL}.
    error MintAmountExceedsCap(uint256 requested, uint256 cap);

    constructor() ERC20("Mock USDC", "mUSDC") { }

    /// @inheritdoc ERC20
    function decimals() public pure override returns (uint8) {
        return DECIMALS;
    }

    /// @notice Permissionless faucet mint, capped at {MAX_MINT_PER_CALL} per call.
    /// @param to recipient of the minted tokens
    /// @param amount base units to mint (must be <= {MAX_MINT_PER_CALL})
    function mint(address to, uint256 amount) external {
        if (amount > MAX_MINT_PER_CALL) {
            revert MintAmountExceedsCap(amount, MAX_MINT_PER_CALL);
        }
        _mint(to, amount);
    }

    /// @notice Convenience faucet: mints {FAUCET_AMOUNT} to `msg.sender`.
    function faucet() external {
        _mint(msg.sender, FAUCET_AMOUNT);
    }
}
