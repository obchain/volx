// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { IERC20 } from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import { VolXPerp } from "../../src/VolXPerp.sol";

/// @notice Test-only subclass exposing the internal vault hooks and a settable
/// reserve, standing in for the position logic that #89/#90 will add.
contract VolXPerpHarness is VolXPerp {
    uint256 private _reserved;

    constructor(IERC20 asset_) VolXPerp(asset_) { }

    function reservedAssets() public view override returns (uint256) {
        return _reserved;
    }

    function setReserved(uint256 r) external {
        _reserved = r;
    }

    /// @notice Simulate a trader loss / fee crediting the vault.
    function creditVault(uint256 amount) external {
        _increaseTotalAssets(amount);
    }

    /// @notice Simulate a trader win debiting the vault.
    function debitVault(uint256 amount) external {
        _decreaseTotalAssets(amount);
    }
}
