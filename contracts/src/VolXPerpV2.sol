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

/// @title VolXPerpV2
/// @notice gTrade-style synthetic volatility perp (v2). Adds two things over v1:
///  1. **Borrowing fee ("funding")** — a continuous fee on open notional that
///     accrues over time and is paid to the LP vault on close/liquidation. The
///     perp settles directly on the oracle (no orderbook mark), so classic
///     mark-vs-index funding is undefined; a time-based borrow fee is the
///     correct analog for a single-vault counterparty model and is what makes
///     holding leverage cost over time. The rate is owner-settable.
///  2. **Conditional orders** — limit-open, take-profit and stop-loss orders
///     that a keeper executes when the oracle price crosses the trigger.
/// Same vault accounting + solvency model as v1 (loss capped at collateral,
/// winning-long gain capped at notional). Testnet demo only (Sepolia), not audited.
contract VolXPerpV2 is ERC20, ReentrancyGuard, Ownable {
    using SafeERC20 for IERC20;
    using SafeCast for uint256;
    using SafeCast for int256;

    IERC20 public immutable asset;
    VolXOracle public immutable oracle;
    uint8 private immutable _assetDecimals;

    uint256 internal _totalAssets;

    uint256 public constant MAX_LEVERAGE = 10;
    uint256 public constant OPEN_FEE_BPS = 10; // 0.1%
    uint256 public constant CLOSE_FEE_BPS = 10; // 0.1%
    uint256 public constant LIQ_THRESHOLD_BPS = 8000; // 80%
    uint256 public constant LIQ_REWARD_BPS = 100; // 1%
    uint256 public constant BPS = 10_000;
    uint256 public constant SECONDS_PER_DAY = 86_400;

    /// @notice Max borrow-fee rate the owner can set: 1000 bps/day (10%/day).
    uint256 public constant MAX_FUNDING_BPS_PER_DAY = 1000;

    /// @notice Borrowing fee charged on notional, in bps per day. Owner-settable
    /// (a keeper can raise it when open interest is one-sided). Default 30 = 0.3%/day.
    uint256 public fundingBpsPerDay = 30;

    enum FeeKind {
        Open,
        Close,
        Funding
    }

    struct Position {
        address trader;
        VolXOracle.Index index;
        bool isLong;
        uint256 collateral;
        uint256 leverage;
        uint256 entryPrice;
        uint256 openedAt;
    }

    mapping(uint256 => Position) public positions;
    uint256 public nextPositionId;
    uint256 public totalReserved;

    // --- conditional orders -------------------------------------------------

    enum OrderKind {
        LimitOpen,
        TakeProfit,
        StopLoss
    }

    /// @param trader order owner
    /// @param kind limit-open / take-profit / stop-loss
    /// @param index index the order trades
    /// @param isLong side (for LimitOpen) / mirror of the target position
    /// @param collateral escrowed collateral (LimitOpen only; 0 for TP/SL)
    /// @param leverage leverage (LimitOpen only)
    /// @param triggerPrice oracle value (1e8) that arms the order
    /// @param triggerAbove fire when mark >= triggerPrice (true) or <= (false)
    /// @param positionId target position (TP/SL only; 0 for LimitOpen)
    struct Order {
        address trader;
        OrderKind kind;
        VolXOracle.Index index;
        bool isLong;
        uint256 collateral;
        uint256 leverage;
        uint256 triggerPrice;
        bool triggerAbove;
        uint256 positionId;
    }

    mapping(uint256 => Order) public orders;
    uint256 public nextOrderId;

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
    error NotLiquidatable(uint256 id, uint256 lossPlusFunding, uint256 threshold);
    error FundingRateTooHigh(uint256 bpsPerDay, uint256 max);
    error OrderNotFound(uint256 id);
    error NotOrderOwner(address caller, address owner);
    error ZeroTrigger();
    error OrderNotTriggered(uint256 id, uint256 mark, uint256 trigger, bool above);

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
        uint256 funding,
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
    event FundingRateUpdated(uint256 previousBpsPerDay, uint256 newBpsPerDay);
    event OrderPlaced(
        uint256 indexed id,
        address indexed trader,
        OrderKind kind,
        uint256 triggerPrice,
        bool triggerAbove
    );
    event OrderCancelled(uint256 indexed id, address indexed trader);
    event OrderExecuted(uint256 indexed id, address indexed executor, uint256 resultId);

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

    // --- vault --------------------------------------------------------------

    function totalAssets() public view returns (uint256) {
        return _totalAssets;
    }

    function reservedAssets() public view returns (uint256) {
        return totalReserved;
    }

    function availableAssets() public view returns (uint256) {
        return _totalAssets > totalReserved ? _totalAssets - totalReserved : 0;
    }

    function convertToShares(uint256 assets) public view returns (uint256) {
        uint256 supply = totalSupply();
        if (supply == 0) return assets;
        if (_totalAssets == 0) revert VaultInsolvent();
        return Math.mulDiv(assets, supply, _totalAssets);
    }

    function convertToAssets(uint256 shares) public view returns (uint256) {
        uint256 supply = totalSupply();
        if (supply == 0) return shares;
        return Math.mulDiv(shares, _totalAssets, supply);
    }

    function deposit(uint256 assets) external nonReentrant returns (uint256 shares) {
        if (assets == 0) revert ZeroAssets();
        shares = convertToShares(assets);
        if (shares == 0) revert ZeroShares();
        _totalAssets += assets;
        _mint(msg.sender, shares);
        asset.safeTransferFrom(msg.sender, address(this), assets);
        emit Deposit(msg.sender, assets, shares);
    }

    function withdraw(uint256 shares) external nonReentrant returns (uint256 assets) {
        if (shares == 0) revert ZeroShares();
        uint256 bal = balanceOf(msg.sender);
        if (bal < shares) revert InsufficientShares(bal, shares);
        assets = convertToAssets(shares);
        if (assets == 0) revert ZeroAssets();
        uint256 available = availableAssets();
        if (assets > available) revert WithdrawExceedsAvailable(assets, available);
        _totalAssets -= assets;
        _burn(msg.sender, shares);
        asset.safeTransfer(msg.sender, assets);
        emit Withdraw(msg.sender, assets, shares);
    }

    // --- funding ------------------------------------------------------------

    /// @notice Set the borrowing-fee rate (bps/day). Owner only.
    function setFundingRate(uint256 bpsPerDay) external onlyOwner {
        if (bpsPerDay > MAX_FUNDING_BPS_PER_DAY) {
            revert FundingRateTooHigh(bpsPerDay, MAX_FUNDING_BPS_PER_DAY);
        }
        emit FundingRateUpdated(fundingBpsPerDay, bpsPerDay);
        fundingBpsPerDay = bpsPerDay;
    }

    /// @notice Borrow fee accrued by a position so far: notional * rate * elapsed.
    function accruedFunding(uint256 id) public view returns (uint256) {
        Position memory p = positions[id];
        if (p.trader == address(0)) return 0;
        return _funding(p);
    }

    function _funding(Position memory p) private view returns (uint256) {
        uint256 elapsed = block.timestamp - p.openedAt;
        uint256 notional = p.collateral * p.leverage;
        return Math.mulDiv(notional * fundingBpsPerDay, elapsed, BPS * SECONDS_PER_DAY);
    }

    // --- positions ----------------------------------------------------------

    function openPosition(VolXOracle.Index index, bool isLong, uint256 collateral, uint256 leverage)
        external
        nonReentrant
        returns (uint256 id)
    {
        // Pull collateral, then open against it.
        asset.safeTransferFrom(msg.sender, address(this), collateral);
        id = _open(msg.sender, index, isLong, collateral, leverage);
    }

    /// @dev Open a position assuming `collateral` is ALREADY held by this
    /// contract (pulled by the caller path — direct open or escrowed limit order).
    function _open(
        address trader,
        VolXOracle.Index index,
        bool isLong,
        uint256 collateral,
        uint256 leverage
    ) internal returns (uint256 id) {
        if (collateral == 0) revert ZeroCollateral();
        if (leverage == 0 || leverage > MAX_LEVERAGE) revert InvalidLeverage(leverage);

        (uint64 price,,) = oracle.getPriceChecked(index);
        uint256 entryPrice = uint256(price);

        uint256 openFee = Math.mulDiv(collateral * leverage, OPEN_FEE_BPS, BPS);
        if (collateral <= openFee) revert CollateralBelowOpenFee(collateral, openFee);

        uint256 working = collateral - openFee;
        uint256 notional = working * leverage;

        id = nextPositionId++;
        positions[id] = Position({
            trader: trader,
            index: index,
            isLong: isLong,
            collateral: working,
            leverage: leverage,
            entryPrice: entryPrice,
            openedAt: block.timestamp
        });
        totalReserved += notional;
        _increaseTotalAssets(openFee);

        emit PositionOpened(
            id, trader, index, isLong, working, leverage, entryPrice, notional, openFee
        );
        emit FeeCollected(id, FeeKind.Open, openFee);
    }

    function closePosition(uint256 id) external nonReentrant {
        Position memory p = positions[id];
        if (p.trader == address(0)) revert PositionNotFound(id);
        if (p.trader != msg.sender) revert NotPositionOwner(msg.sender, p.trader);
        _close(id);
    }

    /// @dev Settle a position against the vault: PnL (price) minus accrued
    /// funding, minus close fee; loss capped at collateral, gain capped at
    /// notional (inside _pnl). Funding + loss + fees accrue to the vault.
    function _close(uint256 id) internal {
        Position memory p = positions[id];
        (uint64 mark,,) = oracle.getPriceChecked(p.index);
        uint256 markPrice = uint256(mark);
        uint256 notional = p.collateral * p.leverage;
        int256 pnl = _pnl(p, markPrice, notional);
        uint256 funding = _funding(p);

        // Equity after price PnL and the borrow fee, floored at 0.
        int256 raw = p.collateral.toInt256() + pnl - funding.toInt256();
        uint256 equity = raw > 0 ? raw.toUint256() : 0;
        uint256 closeFee = Math.min(Math.mulDiv(notional, CLOSE_FEE_BPS, BPS), equity);
        uint256 payout = equity - closeFee;

        if (p.collateral >= payout) {
            _increaseTotalAssets(p.collateral - payout);
        } else {
            _decreaseTotalAssets(payout - p.collateral);
        }

        totalReserved -= notional;
        delete positions[id];

        if (payout > 0) asset.safeTransfer(p.trader, payout);
        emit PositionClosed(id, p.trader, markPrice, pnl, funding, payout, closeFee);
        emit FeeCollected(id, FeeKind.Close, closeFee);
        if (funding > 0) emit FeeCollected(id, FeeKind.Funding, funding);
    }

    /// @notice True if the position's loss + accrued funding has reached
    /// {LIQ_THRESHOLD_BPS} of collateral (i.e. equity <= 20% of collateral).
    function isLiquidatable(uint256 id) public view returns (bool) {
        Position memory p = positions[id];
        if (p.trader == address(0)) return false;
        (uint64 mark,,) = oracle.getPrice(p.index);
        int256 pnl = _pnl(p, uint256(mark), p.collateral * p.leverage);
        int256 raw = p.collateral.toInt256() + pnl - _funding(p).toInt256();
        int256 maintenance = Math.mulDiv(p.collateral, BPS - LIQ_THRESHOLD_BPS, BPS).toInt256();
        return raw <= maintenance;
    }

    function liquidate(uint256 id) external nonReentrant {
        Position memory p = positions[id];
        if (p.trader == address(0)) revert PositionNotFound(id);

        (uint64 mark,,) = oracle.getPriceChecked(p.index);
        uint256 markPrice = uint256(mark);
        uint256 notional = p.collateral * p.leverage;
        int256 pnl = _pnl(p, markPrice, notional);
        uint256 funding = _funding(p);

        int256 raw = p.collateral.toInt256() + pnl - funding.toInt256();
        int256 maintenance = Math.mulDiv(p.collateral, BPS - LIQ_THRESHOLD_BPS, BPS).toInt256();
        if (raw > maintenance) {
            uint256 eq = raw > 0 ? raw.toUint256() : 0;
            revert NotLiquidatable(
                id,
                p.collateral - Math.min(eq, p.collateral),
                Math.mulDiv(p.collateral, LIQ_THRESHOLD_BPS, BPS)
            );
        }

        uint256 equity = raw > 0 ? raw.toUint256() : 0;
        uint256 reward = Math.min(Math.mulDiv(p.collateral, LIQ_REWARD_BPS, BPS), equity);
        uint256 vaultGain = p.collateral - reward;

        _increaseTotalAssets(vaultGain);
        totalReserved -= notional;
        delete positions[id];

        if (reward > 0) asset.safeTransfer(msg.sender, reward);
        emit PositionLiquidated(id, p.trader, msg.sender, markPrice, reward, vaultGain);
    }

    /// @notice Live PnL + equity (net of accrued funding) at the raw oracle price.
    function positionValue(uint256 id) external view returns (int256 pnl, uint256 equity) {
        Position memory p = positions[id];
        if (p.trader == address(0)) revert PositionNotFound(id);
        (uint64 mark,,) = oracle.getPrice(p.index);
        pnl = _pnl(p, uint256(mark), p.collateral * p.leverage);
        int256 raw = p.collateral.toInt256() + pnl - _funding(p).toInt256();
        equity = raw > 0 ? raw.toUint256() : 0;
    }

    // --- conditional orders -------------------------------------------------

    /// @notice Place a limit-open order: escrows collateral now, opens the
    /// position when the oracle crosses `triggerPrice`.
    function placeLimitOpen(
        VolXOracle.Index index,
        bool isLong,
        uint256 collateral,
        uint256 leverage,
        uint256 triggerPrice,
        bool triggerAbove
    ) external nonReentrant returns (uint256 id) {
        if (collateral == 0) revert ZeroCollateral();
        if (leverage == 0 || leverage > MAX_LEVERAGE) revert InvalidLeverage(leverage);
        if (triggerPrice == 0) revert ZeroTrigger();
        asset.safeTransferFrom(msg.sender, address(this), collateral);
        id = nextOrderId++;
        orders[id] = Order({
            trader: msg.sender,
            kind: OrderKind.LimitOpen,
            index: index,
            isLong: isLong,
            collateral: collateral,
            leverage: leverage,
            triggerPrice: triggerPrice,
            triggerAbove: triggerAbove,
            positionId: 0
        });
        emit OrderPlaced(id, msg.sender, OrderKind.LimitOpen, triggerPrice, triggerAbove);
    }

    /// @notice Attach a take-profit or stop-loss to a position you own.
    function placeStop(uint256 positionId, uint256 triggerPrice, bool triggerAbove, bool takeProfit)
        external
        returns (uint256 id)
    {
        Position memory p = positions[positionId];
        if (p.trader == address(0)) revert PositionNotFound(positionId);
        if (p.trader != msg.sender) revert NotPositionOwner(msg.sender, p.trader);
        if (triggerPrice == 0) revert ZeroTrigger();
        id = nextOrderId++;
        orders[id] = Order({
            trader: msg.sender,
            kind: takeProfit ? OrderKind.TakeProfit : OrderKind.StopLoss,
            index: p.index,
            isLong: p.isLong,
            collateral: 0,
            leverage: 0,
            triggerPrice: triggerPrice,
            triggerAbove: triggerAbove,
            positionId: positionId
        });
        emit OrderPlaced(
            id,
            msg.sender,
            takeProfit ? OrderKind.TakeProfit : OrderKind.StopLoss,
            triggerPrice,
            triggerAbove
        );
    }

    /// @notice Cancel an order you own. Refunds escrowed collateral (LimitOpen).
    function cancelOrder(uint256 id) external nonReentrant {
        Order memory o = orders[id];
        if (o.trader == address(0)) revert OrderNotFound(id);
        if (o.trader != msg.sender) revert NotOrderOwner(msg.sender, o.trader);
        uint256 refund = o.kind == OrderKind.LimitOpen ? o.collateral : 0;
        delete orders[id];
        if (refund > 0) asset.safeTransfer(o.trader, refund);
        emit OrderCancelled(id, o.trader);
    }

    /// @notice Execute a triggered order (permissionless — keepers call this).
    /// Checks the oracle price meets the trigger, then opens (LimitOpen) or
    /// closes (TP/SL) the position. A TP/SL whose position is already gone is
    /// simply cleared.
    /// @return resultId the new position id (LimitOpen) or the closed id (TP/SL)
    function executeOrder(uint256 id) external nonReentrant returns (uint256 resultId) {
        Order memory o = orders[id];
        if (o.trader == address(0)) revert OrderNotFound(id);

        (uint64 mark,,) = oracle.getPriceChecked(o.index);
        uint256 markPrice = uint256(mark);
        bool triggered = o.triggerAbove ? markPrice >= o.triggerPrice : markPrice <= o.triggerPrice;
        if (!triggered) revert OrderNotTriggered(id, markPrice, o.triggerPrice, o.triggerAbove);

        if (o.kind == OrderKind.LimitOpen) {
            // Collateral already escrowed at placement.
            delete orders[id];
            resultId = _open(o.trader, o.index, o.isLong, o.collateral, o.leverage);
        } else {
            // TP/SL: close the target position if it still exists + is owned by
            // the order's trader (it may have been closed/liquidated already).
            Position memory p = positions[o.positionId];
            delete orders[id];
            if (p.trader == o.trader) {
                _close(o.positionId);
            }
            resultId = o.positionId;
        }
        emit OrderExecuted(id, msg.sender, resultId);
    }

    // --- internal accounting hooks ------------------------------------------

    function _increaseTotalAssets(uint256 amount) internal {
        _totalAssets += amount;
    }

    function _decreaseTotalAssets(uint256 amount) internal {
        _totalAssets -= amount;
    }

    /// @dev Signed PnL = notional*(mark-entry)/entry, negated for shorts, capped
    /// at +notional (vault solvency). entryPrice is non-zero (oracle rejects 0).
    function _pnl(Position memory p, uint256 mark, uint256 notional) private pure returns (int256) {
        int256 entry = p.entryPrice.toInt256();
        int256 diff = mark.toInt256() - entry;
        int256 pnl = notional.toInt256() * diff / entry;
        if (!p.isLong) pnl = -pnl;
        int256 cap = notional.toInt256();
        return pnl > cap ? cap : pnl;
    }
}
