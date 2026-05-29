// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { Ownable } from "@openzeppelin/contracts/access/Ownable.sol";

/// @title VolXOracle
/// @notice On-chain price store for the VolX volatility indices (BVOL, EVOL).
/// An off-chain keeper pushes the live index value computed by the VolX engine;
/// the perp reads it to settle positions. Chainlink-style semantics: value +
/// timestamp + a freshness guard. There is no on-chain VIX math — the value is
/// authoritative from the off-chain engine. Testnet demo only (Sepolia).
/// @dev Each index's data packs into a single storage slot
/// (`uint64 + uint64 + uint32 = 160 bits`), so an update is one SSTORE per index.
contract VolXOracle is Ownable {
    /// @notice The two indices this oracle serves.
    enum Index {
        BVOL,
        EVOL
    }

    /// @notice Packed price record. One storage slot.
    /// @param value index value, fixed-point at {VALUE_SCALE} (1e8)
    /// @param updatedAt unix seconds of the last write (0 = never set)
    /// @param confidence engine confidence, fixed-point at {CONFIDENCE_SCALE} (1e6, range [0, 1e6])
    struct PriceData {
        uint64 value;
        uint64 updatedAt;
        uint32 confidence;
    }

    /// @notice Fixed-point scale for index values (8 decimals).
    uint256 public constant VALUE_SCALE = 1e8;

    /// @notice Fixed-point scale for confidence (1e6 == 1.0); values must be <= this.
    uint32 public constant CONFIDENCE_SCALE = 1e6;

    /// @notice A price older than this (seconds) is considered stale by
    /// {getPriceChecked}. Set above the keeper's 30-minute heartbeat to leave
    /// margin for missed beats / gas-price pauses.
    uint256 public constant MAX_STALENESS = 1 hours;

    /// @notice Address allowed to push prices. Settable by the owner.
    address public keeper;

    /// @dev index => packed price record.
    mapping(Index => PriceData) private _prices;

    /// @notice Emitted on every price write.
    event PriceUpdated(Index indexed index, uint64 value, uint32 confidence, uint64 updatedAt);

    /// @notice Emitted when the keeper address changes.
    event KeeperUpdated(address indexed previousKeeper, address indexed newKeeper);

    error NotKeeper(address caller);
    error ZeroAddress();
    error ConfidenceTooHigh(uint32 confidence, uint32 max);
    error ZeroValue(Index index);
    error PriceNeverSet(Index index);
    error StalePrice(Index index, uint64 updatedAt, uint256 nowTs, uint256 maxStaleness);

    modifier onlyKeeper() {
        _checkKeeper();
        _;
    }

    /// @dev Extracted from the modifier so the check is not inlined at every
    /// use site (smaller bytecode).
    function _checkKeeper() private view {
        if (msg.sender != keeper) revert NotKeeper(msg.sender);
    }

    /// @param keeper_ initial keeper (the off-chain pusher); owner is the deployer
    constructor(address keeper_) Ownable(msg.sender) {
        if (keeper_ == address(0)) revert ZeroAddress();
        keeper = keeper_;
        emit KeeperUpdated(address(0), keeper_);
    }

    /// @notice Rotate the keeper address.
    /// @param newKeeper the new keeper (non-zero)
    function setKeeper(address newKeeper) external onlyOwner {
        if (newKeeper == address(0)) revert ZeroAddress();
        emit KeeperUpdated(keeper, newKeeper);
        keeper = newKeeper;
    }

    /// @notice Push both index values in one transaction.
    /// @param bvol BVOL value at {VALUE_SCALE}
    /// @param bvolConf BVOL confidence at {CONFIDENCE_SCALE}
    /// @param evol EVOL value at {VALUE_SCALE}
    /// @param evolConf EVOL confidence at {CONFIDENCE_SCALE}
    function updateBoth(uint64 bvol, uint32 bvolConf, uint64 evol, uint32 evolConf)
        external
        onlyKeeper
    {
        _set(Index.BVOL, bvol, bvolConf);
        _set(Index.EVOL, evol, evolConf);
    }

    /// @notice Push a single index. Lets the keeper refresh one feed when the
    /// other is temporarily unavailable, instead of letting both go stale.
    /// @param index which index to write
    /// @param value index value at {VALUE_SCALE}
    /// @param confidence confidence at {CONFIDENCE_SCALE}
    function updateOne(Index index, uint64 value, uint32 confidence) external onlyKeeper {
        _set(index, value, confidence);
    }

    /// @notice Raw price read. Does NOT check staleness — callers that care
    /// about freshness must use {getPriceChecked}.
    function getPrice(Index index)
        external
        view
        returns (uint64 value, uint64 updatedAt, uint32 confidence)
    {
        PriceData storage p = _prices[index];
        return (p.value, p.updatedAt, p.confidence);
    }

    /// @notice Freshness-guarded price read. Reverts if the index was never
    /// set or if it is older than {MAX_STALENESS}.
    function getPriceChecked(Index index)
        external
        view
        returns (uint64 value, uint64 updatedAt, uint32 confidence)
    {
        PriceData storage p = _prices[index];
        if (p.updatedAt == 0) revert PriceNeverSet(index);
        if (block.timestamp - p.updatedAt > MAX_STALENESS) {
            revert StalePrice(index, p.updatedAt, block.timestamp, MAX_STALENESS);
        }
        return (p.value, p.updatedAt, p.confidence);
    }

    /// @dev Validate + write a single index record (one SSTORE).
    function _set(Index index, uint64 value, uint32 confidence) private {
        if (value == 0) revert ZeroValue(index);
        if (confidence > CONFIDENCE_SCALE) revert ConfidenceTooHigh(confidence, CONFIDENCE_SCALE);

        // Safe cast: uint64 seconds overflows far beyond any practical date; testnet scope.
        uint64 ts = uint64(block.timestamp);
        _prices[index] = PriceData({ value: value, updatedAt: ts, confidence: confidence });
        emit PriceUpdated(index, value, confidence, ts);
    }
}
