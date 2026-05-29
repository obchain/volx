// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { ERC20 } from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import { IERC20 } from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import { IERC20Metadata } from "@openzeppelin/contracts/token/ERC20/extensions/IERC20Metadata.sol";
import { SafeERC20 } from "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import { ReentrancyGuard } from "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import { Ownable } from "@openzeppelin/contracts/access/Ownable.sol";
import { Math } from "@openzeppelin/contracts/utils/math/Math.sol";
import { SafeCast } from "@openzeppelin/contracts/utils/math/SafeCast.sol";
import { VolXOracle } from "./VolXOracle.sol";

/// @title VolXPerp
/// @notice gTrade-style synthetic volatility perp. A single pot of LP-supplied
/// collateral (MockUSDC) is the counterparty to every trade: LPs deposit and
/// receive `vxLP` shares; the vault's value grows by trader losses + fees and
/// shrinks by trader wins. Traders open leveraged long/short bets on the BVOL/
/// EVOL indices priced by {VolXOracle}, settling PnL against the vault on close
/// or liquidation. Loss is capped at collateral and a winning long's gain is
/// capped at notional, so the vault stays solvent against its reserve.
/// Testnet demo only (Sepolia), not audited. No funding rate in v1.
/// @dev ERC4626-style share math, but `totalAssets` is tracked in an internal
/// accounting variable (not `token.balanceOf(this)`) so that direct token
/// donations cannot inflate the share price (classic ERC4626 donation attack),
/// and so realized trader PnL can be credited/debited without a token move.
contract VolXPerp is ERC20, ReentrancyGuard, Ownable {
    using SafeERC20 for IERC20;
    using SafeCast for uint256;
    using SafeCast for int256;

    /// @notice The collateral token (MockUSDC, 6 decimals).
    IERC20 public immutable asset;

    /// @notice Price source for entry/mark prices.
    VolXOracle public immutable oracle;

    /// @dev Share token decimals, mirrored from the underlying asset.
    uint8 private immutable _assetDecimals;

    /// @dev Internal accounting of vault-managed collateral. Diverges from
    /// `asset.balanceOf(this)` by exactly any donated tokens + open trader
    /// collateral (held but not LP-owned); both ignored on purpose.
    uint256 internal _totalAssets;

    /// @notice Max leverage a trader may open (demo default 10x).
    uint256 public constant MAX_LEVERAGE = 10;

    /// @notice Open fee in basis points (0.1%).
    uint256 public constant OPEN_FEE_BPS = 10;

    /// @notice Close fee in basis points (0.1%).
    uint256 public constant CLOSE_FEE_BPS = 10;

    /// @notice Liquidation trigger: a position is liquidatable once its
    /// unrealized loss reaches this fraction of collateral (80%).
    uint256 public constant LIQ_THRESHOLD_BPS = 8000;

    /// @notice Liquidator reward, as a fraction of collateral (1%), paid from
    /// the liquidated position's remaining equity.
    uint256 public constant LIQ_REWARD_BPS = 100;

    /// @notice Basis-point denominator.
    uint256 public constant BPS = 10_000;

    /// @notice Categorises a {FeeCollected} event. Only fees the vault retains
    /// (open + close) — the liquidator reward leaves the vault and is reported
    /// separately by {PositionLiquidated}.
    enum FeeKind {
        Open,
        Close
    }

    /// @notice An open leveraged bet against the vault.
    /// @param trader position owner
    /// @param index which volatility index the bet tracks
    /// @param isLong true = profits when the index rises
    /// @param collateral working collateral after the open fee (6dp)
    /// @param leverage integer leverage in [1, MAX_LEVERAGE]
    /// @param entryPrice oracle value at open (1e8 scale)
    /// @param openedAt unix seconds at open
    struct Position {
        address trader;
        VolXOracle.Index index;
        bool isLong;
        uint256 collateral;
        uint256 leverage;
        uint256 entryPrice;
        uint256 openedAt;
    }

    /// @notice id => position. `trader == address(0)` means absent.
    mapping(uint256 => Position) public positions;

    /// @notice Next position id to assign (monotonic).
    uint256 public nextPositionId;

    /// @notice Sum of open-position notional — collateral reserved against the
    /// vault so LPs cannot withdraw collateral that may be owed to traders.
    uint256 public totalReserved;

    error ZeroAssets();
    error ZeroShares();
    error ZeroAddress();
    error InsufficientShares(uint256 have, uint256 want);
    error WithdrawExceedsAvailable(uint256 requested, uint256 available);
    error VaultInsolvent();
    error ZeroCollateral();
    error InvalidLeverage(uint256 leverage);
    error CollateralBelowOpenFee(uint256 collateral, uint256 openFee);
    error PositionNotFound(uint256 id);
    error NotPositionOwner(address caller, address owner);
    error NotLiquidatable(uint256 id, uint256 loss, uint256 threshold);

    event Deposit(address indexed caller, uint256 assets, uint256 shares);
    event Withdraw(address indexed caller, uint256 assets, uint256 shares);
    event PositionOpened(
        uint256 indexed id,
        address indexed trader,
        VolXOracle.Index indexed index,
        bool isLong,
        uint256 collateral,
        uint256 leverage,
        uint256 entryPrice,
        uint256 notional,
        uint256 openFee
    );
    event PositionClosed(
        uint256 indexed id,
        address indexed trader,
        uint256 markPrice,
        int256 pnl,
        uint256 payout,
        uint256 closeFee
    );
    event PositionLiquidated(
        uint256 indexed id,
        address indexed trader,
        address indexed liquidator,
        uint256 markPrice,
        uint256 reward,
        uint256 vaultGain
    );
    event FeeCollected(uint256 indexed id, FeeKind kind, uint256 amount);

    /// @param asset_ collateral token (must expose `decimals()`)
    /// @param oracle_ price source
    constructor(IERC20 asset_, VolXOracle oracle_) ERC20("VolX LP", "vxLP") Ownable(msg.sender) {
        if (address(asset_) == address(0) || address(oracle_) == address(0)) revert ZeroAddress();
        asset = asset_;
        oracle = oracle_;
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

    /// @notice Collateral reserved against open interest (sum of open notional)
    /// and therefore not withdrawable by LPs.
    function reservedAssets() public view virtual returns (uint256) {
        return totalReserved;
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
        // Dust guard: never burn shares for zero payout (floor rounding, or an
        // insolvent vault with _totalAssets==0, can make this 0).
        if (assets == 0) revert ZeroAssets();
        uint256 available = availableAssets();
        if (assets > available) revert WithdrawExceedsAvailable(assets, available);

        // Effects before interaction.
        _totalAssets -= assets;
        _burn(msg.sender, shares);

        asset.safeTransfer(msg.sender, assets);
        emit Withdraw(msg.sender, assets, shares);
    }

    // --- internal accounting hooks (consumed by #89/#90) --------------------
    //
    // INVARIANT for inheriting contracts: `_totalAssets` must stay <=
    // `asset.balanceOf(this)` (donations make the real balance larger, which is
    // benign). To preserve it:
    //   - credit path:  complete `safeTransferFrom(payer, vault, amount)` BEFORE
    //     calling `_increaseTotalAssets(amount)`.
    //   - payout path:  call `_decreaseTotalAssets(amount)` BEFORE
    //     `safeTransfer(recipient, amount)` (effects-before-interaction, exactly
    //     as `withdraw` does).
    // Reversing either order risks an irrecoverable accounting/balance desync.

    /// @dev Credit the vault (trader loss or fee). The corresponding tokens must
    /// already have been transferred in by the caller path; this only moves the
    /// accounting. See the INVARIANT note above.
    function _increaseTotalAssets(uint256 amount) internal {
        _totalAssets += amount;
    }

    /// @dev Debit the vault (trader win to be paid out). Call this BEFORE the
    /// token `safeTransfer`. Reverts on underflow via checked arithmetic if a
    /// position tries to pay more than the vault holds. See the INVARIANT note.
    function _decreaseTotalAssets(uint256 amount) internal {
        _totalAssets -= amount;
    }

    // --- positions ----------------------------------------------------------

    /// @notice Open a leveraged bet on `index`. Pulls `collateral`, records the
    /// oracle price as entry, charges the open fee to the vault, and reserves the
    /// notional. Reverts if the oracle is stale/unset (via {VolXOracle.getPriceChecked}).
    /// @param index volatility index to bet on
    /// @param isLong true to profit on a rise, false on a fall
    /// @param collateral collateral to post (6dp); must exceed the open fee
    /// @param leverage integer leverage in [1, MAX_LEVERAGE]
    /// @return id the new position id
    function openPosition(VolXOracle.Index index, bool isLong, uint256 collateral, uint256 leverage)
        external
        nonReentrant
        returns (uint256 id)
    {
        if (collateral == 0) revert ZeroCollateral();
        if (leverage == 0 || leverage > MAX_LEVERAGE) revert InvalidLeverage(leverage);

        (uint64 price,,) = oracle.getPriceChecked(index); // reverts if stale/unset
        uint256 entryPrice = uint256(price);

        uint256 openFee = Math.mulDiv(collateral * leverage, OPEN_FEE_BPS, BPS);
        // Defensive: unreachable at current constants (max fee = leverage*0.1% =
        // 1% of collateral), but guards `collateral - openFee` if MAX_LEVERAGE or
        // OPEN_FEE_BPS is ever raised.
        if (collateral <= openFee) revert CollateralBelowOpenFee(collateral, openFee);

        uint256 working = collateral - openFee;
        uint256 notional = working * leverage;

        id = nextPositionId++;
        positions[id] = Position({
            trader: msg.sender,
            index: index,
            isLong: isLong,
            collateral: working,
            leverage: leverage,
            entryPrice: entryPrice,
            openedAt: block.timestamp
        });
        totalReserved += notional;

        // Pull collateral in BEFORE crediting the fee, per the hook invariant
        // (tokens must back the accounting bump).
        asset.safeTransferFrom(msg.sender, address(this), collateral);
        _increaseTotalAssets(openFee);

        emit PositionOpened(
            id, msg.sender, index, isLong, working, leverage, entryPrice, notional, openFee
        );
        emit FeeCollected(id, FeeKind.Open, openFee);
    }

    /// @notice Close your position, settling PnL against the vault. Loss is
    /// capped at collateral; a win is paid from the vault. Charges the close fee.
    /// @param id position id (must be owned by the caller)
    function closePosition(uint256 id) external nonReentrant {
        Position memory p = positions[id];
        if (p.trader == address(0)) revert PositionNotFound(id);
        if (p.trader != msg.sender) revert NotPositionOwner(msg.sender, p.trader);

        (uint64 mark,,) = oracle.getPriceChecked(p.index);
        uint256 markPrice = uint256(mark);
        uint256 notional = p.collateral * p.leverage;
        int256 pnl = _pnl(p, markPrice, notional);

        // Trader can never lose more than collateral (raw floored at 0 below);
        // gain is capped at notional inside _pnl so the reserve always covers it.
        int256 raw = p.collateral.toInt256() + pnl;
        uint256 payoutBeforeFee = raw > 0 ? raw.toUint256() : 0;
        uint256 closeFee = Math.min(Math.mulDiv(notional, CLOSE_FEE_BPS, BPS), payoutBeforeFee);
        uint256 payout = payoutBeforeFee - closeFee;

        // Net token movement vs the vault: the position held `p.collateral`; the
        // difference to `payout` is the vault's gain (loss+fees) or its outlay (win).
        if (p.collateral >= payout) {
            _increaseTotalAssets(p.collateral - payout);
        } else {
            _decreaseTotalAssets(payout - p.collateral);
        }

        totalReserved -= notional;
        delete positions[id];

        if (payout > 0) asset.safeTransfer(p.trader, payout);
        emit PositionClosed(id, p.trader, markPrice, pnl, payout, closeFee);
        emit FeeCollected(id, FeeKind.Close, closeFee);
    }

    /// @notice True if `id` is open and its unrealized loss has reached
    /// {LIQ_THRESHOLD_BPS} of collateral. Uses the raw (unchecked) oracle price
    /// so keepers/UIs can poll without reverting on a momentarily stale feed;
    /// {liquidate} itself re-reads with the staleness guard.
    function isLiquidatable(uint256 id) public view returns (bool) {
        Position memory p = positions[id];
        if (p.trader == address(0)) return false;
        (uint64 mark,,) = oracle.getPrice(p.index);
        int256 pnl = _pnl(p, uint256(mark), p.collateral * p.leverage);
        if (pnl >= 0) return false;
        uint256 loss = (-pnl).toUint256();
        return loss >= Math.mulDiv(p.collateral, LIQ_THRESHOLD_BPS, BPS);
    }

    /// @notice Force-close an underwater position (anyone may call). The caller
    /// earns a liquidation reward from the position's remaining equity; the rest
    /// of the collateral goes to the vault. The liquidated trader receives nothing.
    /// @param id position id
    function liquidate(uint256 id) external nonReentrant {
        Position memory p = positions[id];
        if (p.trader == address(0)) revert PositionNotFound(id);

        (uint64 mark,,) = oracle.getPriceChecked(p.index);
        uint256 markPrice = uint256(mark);
        uint256 notional = p.collateral * p.leverage;
        int256 pnl = _pnl(p, markPrice, notional);

        uint256 loss = pnl < 0 ? (-pnl).toUint256() : 0;
        uint256 threshold = Math.mulDiv(p.collateral, LIQ_THRESHOLD_BPS, BPS);
        if (loss < threshold) revert NotLiquidatable(id, loss, threshold);

        // Remaining equity (collateral net of loss); reward carved from it, the
        // rest forfeited to the vault as the liquidation penalty.
        int256 raw = p.collateral.toInt256() + pnl;
        uint256 equity = raw > 0 ? raw.toUint256() : 0;
        uint256 reward = Math.min(Math.mulDiv(p.collateral, LIQ_REWARD_BPS, BPS), equity);
        uint256 vaultGain = p.collateral - reward;

        _increaseTotalAssets(vaultGain);
        totalReserved -= notional;
        delete positions[id];

        if (reward > 0) asset.safeTransfer(msg.sender, reward);
        emit PositionLiquidated(id, p.trader, msg.sender, markPrice, reward, vaultGain);
    }

    /// @notice Live PnL + equity for a position, at the current (raw) oracle
    /// price. View helper for UIs; does not check staleness so it never reverts
    /// on a momentarily stale feed.
    /// @param id position id
    /// @return pnl signed PnL (6dp)
    /// @return equity collateral + pnl, floored at 0 (6dp)
    function positionValue(uint256 id) external view returns (int256 pnl, uint256 equity) {
        Position memory p = positions[id];
        if (p.trader == address(0)) revert PositionNotFound(id);
        (uint64 mark,,) = oracle.getPrice(p.index);
        pnl = _pnl(p, uint256(mark), p.collateral * p.leverage);
        int256 raw = p.collateral.toInt256() + pnl;
        equity = raw > 0 ? raw.toUint256() : 0;
    }

    /// @dev Signed PnL = notional * (mark - entry) / entry, negated for shorts,
    /// then capped at +notional. The cap bounds a winning long's payout (raw
    /// upside is unbounded as the index rises) to the reserve set aside
    /// (`notional`), keeping the vault solvent by construction even after LPs
    /// withdraw down to the reserve floor. A short's raw maximum (at mark=0) is
    /// already exactly +notional, so the cap is a no-op for shorts — gain
    /// treatment is symmetric. `entryPrice` is non-zero (oracle rejects 0).
    function _pnl(Position memory p, uint256 mark, uint256 notional) private pure returns (int256) {
        int256 entry = p.entryPrice.toInt256();
        int256 diff = mark.toInt256() - entry;
        int256 pnl = notional.toInt256() * diff / entry;
        if (!p.isLong) pnl = -pnl;
        int256 cap = notional.toInt256();
        return pnl > cap ? cap : pnl;
    }
}
