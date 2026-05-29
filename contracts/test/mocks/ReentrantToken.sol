// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;

import { ERC20 } from "@openzeppelin/contracts/token/ERC20/ERC20.sol";

interface IVault {
    function withdraw(uint256 shares) external returns (uint256);
}

/// @notice Malicious ERC20 used to prove the vault's reentrancy guard. On every
/// `transfer` (the asset payout in {VolXPerp.withdraw}) it attempts to reenter
/// `withdraw`. A correctly guarded vault makes the reentrant call revert.
contract ReentrantToken is ERC20 {
    IVault public vault;
    bool public attack;

    constructor() ERC20("Reentrant", "RE") { }

    function setVault(address v) external {
        vault = IVault(v);
    }

    function mint(address to, uint256 amount) external {
        _mint(to, amount);
    }

    function armAttack(bool on) external {
        attack = on;
    }

    function _update(address from, address to, uint256 value) internal override {
        super._update(from, to, value);
        // Reenter only on the vault's payout leg (vault -> attacker).
        if (attack && from == address(vault) && address(vault) != address(0)) {
            vault.withdraw(value);
        }
    }
}
