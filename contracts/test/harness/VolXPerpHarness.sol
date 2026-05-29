// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { IERC20 } from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import { VolXPerp } from "../../src/VolXPerp.sol";
import { VolXOracle } from "../../src/VolXOracle.sol";

/// @notice Test-only subclass that lets vault tests drive the reserve and the
/// accounting hooks directly, in isolation from the position logic. `reservedAssets`
/// is overridden to a settable value so the LP-withdrawal guard can be tested
/// without opening real positions.
contract VolXPerpHarness is VolXPerp {
    uint256 private _reserved;

    constructor(IERC20 asset_, VolXOracle oracle_) VolXPerp(asset_, oracle_) { }

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
