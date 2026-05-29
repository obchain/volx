// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { ERC20 } from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import { IERC20 } from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import { IERC20Metadata } from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import { SafeERC20 } from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import { ReentrancyGuard } from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import { Ownable } from "@openzeppelin/contracts/access/Ownable.sol";
import { Math } from "@openzeppelin/contracts/utils/math/Math.sol";

/// @title VolXPerp (LP vault portion)
/// @notice gTrade-style shared collateral vault: a single pot of LP-supplied
/// collateral (MockUSDC) is the counterparty to every trade. LPs deposit and
/// receive `vxLP` shares; the vault's value grows by trader losses + fees and
/// shrinks by trader wins. This contract implements the vault accounting only —
/// position open/close, PnL, liquidation and fees land in #89/#90 and feed the
/// vault through the internal hooks {_increaseTotalAssets} / {_decreaseTotalAssets}.
/// Testnet demo only (Sepolia), not audited.
/// @dev ERC4626-style share math, but `totalAssets` is tracked in an internal
/// accounting variable (not `token.balanceOf(this)`) so that direct token
/// donations cannot inflate the share price (classic ERC4626 donation attack),
/// and so realized trader PnL can be credited/debited without a token move.
contract VolXPerp is ERC20, ReentrancyGuard, Ownable {
    using SafeERC20 for IERC20;

    /// @notice The collateral token (MockUSDC, 6 decimals).
    IERC20 public immutable asset;

    /// @dev Share token decimals, mirrored from the underlying asset.
    uint8 private immutable _assetDecimals;

    /// @dev Internal accounting of vault-managed collateral. Diverges from
    /// `asset.balanceOf(this)` by exactly any donated tokens (ignored on purpose).
    uint256 internal _totalAssets;

    error ZeroAssets();
    error ZeroShares();
    error ZeroAddress();
    error InsufficientShares(uint256 have, uint256 want);
    error WithdrawExceedsAvailable(uint256 requested, uint256 available);
    error VaultInsolvent();

    event Deposit(address indexed caller, uint256 assets, uint256 shares);
    event Withdraw(address indexed caller, uint256 assets, uint256 shares);

    /// @param asset_ collateral token (must expose `decimals()`)
    constructor(IERC20 asset_) ERC20("VolX LP", "vxLP") Ownable(msg.sender) {
        if (address(asset_) == address(0)) revert ZeroAddress();
        asset = asset_;
        _assetDecimals = IERC20Metadata(address(asset_)).decimals();
    }

    /// @inheritdoc ERC20
    function decimals() public view override returns (uint8) {
        return _assetDecimals;
    }

    /// @notice Total collateral the vault accounts for (LP principal +/- realized
    /// trader PnL + fees). Internal accounting; not affected by token donations.
    function totalAssets() public view returns (uint256) {
        return _totalAssets;
    }

    /// @notice Collateral currently reserved against open interest and therefore
    /// not withdrawable by LPs. Stub returns 0 here; #89/#90 override with the
    /// real open-interest margin reserve.
    function reservedAssets() public view virtual returns (uint256) {
        return 0;
    }

    /// @notice Collateral free for LP withdrawal: `totalAssets - reservedAssets`.
    function availableAssets() public view returns (uint256) {
        uint256 reserved = reservedAssets();
        return _totalAssets > reserved ? _totalAssets - reserved : 0;
    }

    /// @notice Shares minted for `assets` at the current exchange rate (floor).
    function convertToShares(uint256 assets) public view returns (uint256) {
        uint256 supply = totalSupply();
        if (supply == 0) return assets; // first deposit: 1:1
        if (_totalAssets == 0) revert VaultInsolvent();
        return Math.mulDiv(assets, supply, _totalAssets);
    }

    /// @notice Assets redeemable for `shares` at the current exchange rate (floor).
    function convertToAssets(uint256 shares) public view returns (uint256) {
        uint256 supply = totalSupply();
        if (supply == 0) return shares;
        return Math.mulDiv(shares, _totalAssets, supply);
    }

    /// @notice Deposit `assets` of collateral, mint `vxLP` shares to the caller.
    /// @param assets collateral amount to deposit (base units)
    /// @return shares shares minted
    function deposit(uint256 assets) external nonReentrant returns (uint256 shares) {
        if (assets == 0) revert ZeroAssets();
        shares = convertToShares(assets);
        if (shares == 0) revert ZeroShares();

        // Effects before the external pull; nonReentrant also guards.
        _totalAssets += assets;
        _mint(msg.sender, shares);

        asset.safeTransferFrom(msg.sender, address(this), assets);
        emit Deposit(msg.sender, assets, shares);
    }

    /// @notice Burn `shares` and return the proportional collateral, provided it
    /// does not dip the vault below {reservedAssets}.
    /// @param shares shares to redeem
    /// @return assets collateral returned (base units)
    function withdraw(uint256 shares) external nonReentrant returns (uint256 assets) {
        if (shares == 0) revert ZeroShares();
        uint256 bal = balanceOf(msg.sender);
        if (bal < shares) revert InsufficientShares(bal, shares);

        assets = convertToAssets(shares);
        uint256 available = availableAssets();
        if (assets > available) revert WithdrawExceedsAvailable(assets, available);

        // Effects before interaction.
        _totalAssets -= assets;
        _burn(msg.sender, shares);

        asset.safeTransfer(msg.sender, assets);
        emit Withdraw(msg.sender, assets, shares);
    }

    // --- internal accounting hooks (consumed by #89/#90) --------------------

    /// @dev Credit the vault (trader loss or fee). Token must already be held
    /// or transferred in by the caller path; this only moves the accounting.
    function _increaseTotalAssets(uint256 amount) internal {
        _totalAssets += amount;
    }

    /// @dev Debit the vault (trader win paid out). Reverts on underflow via
    /// checked arithmetic if a position tries to pay more than the vault holds.
    function _decreaseTotalAssets(uint256 amount) internal {
        _totalAssets -= amount;
    }
}
