// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

module Sui::ValidatorSet {
    use Std::Option::{Self, Option};
    use Std::Vector;

    use Sui::Coin::Coin;
    use Sui::SUI::SUI;
    use Sui::TxContext::{Self, TxContext};
    use Sui::Validator::{Self, Validator};

    friend Sui::SuiSystem;

    #[test_only]
    friend Sui::ValidatorSetTests;

    struct ValidatorSet has store {
        /// Total amount of stake from all active validators, at the beginning of the epoch.
        total_stake: u64,

        /// The amount of accumulated stake to reach a quorum among all active validators.
        /// This is always 2/3 of total stake. Keep it here to reduce potential inconsistencies
        /// among validators.
        quorum_stake_threshold: u64,

        /// The current list of active validators.
        active_validators: vector<Validator>,

        /// List of new validator candidates added during the current epoch.
        /// They will be processed at the end of the epoch.
        pending_validators: vector<Validator>,

        /// Removal requests from the validators. Each element is an index
        /// pointing to `active_validators`.
        pending_removals: vector<u64>,
    }

    public(friend) fun new(init_active_validators: vector<Validator>): ValidatorSet {
        let (total_stake, quorum_stake_threshold) = calculate_total_stake_and_quorum_threshold(&init_active_validators);
        ValidatorSet {
            total_stake,
            quorum_stake_threshold,
            active_validators: init_active_validators,
            pending_validators: Vector::empty(),
            pending_removals: Vector::empty(),
        }
    }

    /// Get the total number of candidates that might become validators in the next epoch.
    public(friend) fun get_total_validator_candidate_count(self: &ValidatorSet): u64 {
        Vector::length(&self.active_validators)
            + Vector::length(&self.pending_validators)
            - Vector::length(&self.pending_removals)
    }

    /// Called by `SuiSystem`, add a new validator to `pending_validators`, which will be
    /// processed at the end of epoch.
    public(friend) fun request_add_validator(self: &mut ValidatorSet, validator: Validator) {
        assert!(
            !contains_duplicate_validator(&self.active_validators, &validator)
                && !contains_duplicate_validator(&self.pending_validators, &validator),
            0
        );
        Vector::push_back(&mut self.pending_validators, validator);
    }

    /// Called by `SuiSystem`, to remove a validator.
    /// The index of the validator is added to `pending_removals` and
    /// will be processed at the end of epoch.
    /// Only an active validator can request to be removed.
    public(friend) fun request_remove_validator(
        self: &mut ValidatorSet,
        ctx: &TxContext,
    ) {
        let validator_address = TxContext::sender(ctx);
        let validator_index_opt = find_validator(&self.active_validators, validator_address);
        assert!(Option::is_some(&validator_index_opt), 0);
        let validator_index = Option::extract(&mut validator_index_opt);
        assert!(
            !Vector::contains(&self.pending_removals, &validator_index),
            0
        );
        Vector::push_back(&mut self.pending_removals, validator_index);
    }

    /// Called by `SuiSystem`, to add more stake to a validator.
    /// The new stake will be added to the validator's pending stake, which will be processed
    /// at the end of epoch.
    /// The total stake of the validator cannot exceed `max_validator_stake` with the `new_stake`.
    public(friend) fun request_add_stake(
        self: &mut ValidatorSet,
        new_stake: Coin<SUI>,
        max_validator_stake: u64,
        ctx: &TxContext,
    ) {
        let validator_address = TxContext::sender(ctx);
        let validator_index_opt = find_validator(&self.active_validators, validator_address);
        assert!(Option::is_some(&validator_index_opt), 0);
        let validator_index = Option::extract(&mut validator_index_opt);
        let validator = Vector::borrow_mut(&mut self.active_validators, validator_index);
        Validator::request_add_stake(validator, new_stake, max_validator_stake);
    }

    /// Called by `SuiSystem`, to withdraw stake from a validator.
    /// We send a withdraw request to the validator which will be processed at the end of epoch.
    /// The remaining stake of the validator cannot be lower than `min_validator_stake`.
    public(friend) fun request_withdraw_stake(
        self: &mut ValidatorSet,
        withdraw_amount: u64,
        min_validator_stake: u64,
        ctx: &TxContext,
    ) {
        let validator_address = TxContext::sender(ctx);
        let validator_index_opt = find_validator(&self.active_validators, validator_address);
        assert!(Option::is_some(&validator_index_opt), 0);
        let validator_index = Option::extract(&mut validator_index_opt);
        let validator = Vector::borrow_mut(&mut self.active_validators, validator_index);
        Validator::request_withdraw_stake(validator, withdraw_amount, min_validator_stake);
    }

    public(friend) fun is_active_validator(
        self: &ValidatorSet,
        validator_address: address,
    ): bool {
        Option::is_some(&find_validator(&self.active_validators, validator_address))
    }

    /// Update the validator set at the end of epoch.
    /// It does the following things:
    ///   1. Distribute stake award.
    ///   2. Process pending stake deposits and withdraws for each validator (`adjust_stake`).
    ///   3. Process pending validator application and withdraws.
    ///   4. At the end, we calculate the total stake for the new epoch.
    public(friend) fun advance_epoch(
        self: &mut ValidatorSet,
        ctx: &mut TxContext,
    ) {
        // TODO: Distribute stake rewards.

        // This needs to come before adjust_stake since some of the
        // pending validators may also have pending deposits.
        process_pending_validators(&mut self.active_validators, &mut self.pending_validators);

        adjust_stake(&mut self.active_validators, ctx);

        process_pending_removals(&mut self.active_validators, &mut self.pending_removals);

        let (total_stake, quorum_stake_threshold) = calculate_total_stake_and_quorum_threshold(&self.active_validators);
        self.total_stake = total_stake;
        self.quorum_stake_threshold = quorum_stake_threshold;
    }

    public fun total_stake(self: &ValidatorSet): u64 {
        self.total_stake
    }

    /// Checks whether a duplicate of `new_validator` is already in `validators`.
    /// Two validators duplicate if they share the same sui_address or same IP or same name.
    fun contains_duplicate_validator(validators: &vector<Validator>, new_validator: &Validator): bool {
        let len = Vector::length(validators);
        let i = 0;
        while (i < len) {
            let v = Vector::borrow(validators, i);
            if (Validator::is_duplicate(v, new_validator)) {
                return true
            };
            i = i + 1;
        };
        false
    }

    /// Find validator by `validator_address`, in `validators`.
    /// Returns (true, index) if the validator is found, and the index is its index in the list.
    /// If not found, returns (false, 0).
    fun find_validator(validators: &vector<Validator>, validator_address: address): Option<u64> {
        let length = Vector::length(validators);
        let i = 0;
        while (i < length) {
            let v = Vector::borrow(validators, i);
            if (Validator::sui_address(v) == validator_address) {
                return Option::some(i)
            };
            i = i + 1;
        };
        Option::none()
    }

    /// Process the pending withdraw requests. For each pending request, the validator
    /// is removed from `validators` and sent back to the address of the validator.
    fun process_pending_removals(validators: &mut vector<Validator>, withdraw_list: &mut vector<u64>) {
        sort_removal_list(withdraw_list);
        while (!Vector::is_empty(withdraw_list)) {
            let index = Vector::pop_back(withdraw_list);
            let validator = Vector::remove(validators, index);
            Validator::destroy(validator);
        }
    }

    /// Process the pending new validators. They are simply inserted into `validators`.
    fun process_pending_validators(validators: &mut vector<Validator>, pending_validators: &mut vector<Validator>) {
        while (!Vector::is_empty(pending_validators)) {
            let v = Vector::pop_back(pending_validators);
            Vector::push_back(validators, v);
        }
    }

    /// Sort all the pending removal indexes.
    fun sort_removal_list(withdraw_list: &mut vector<u64>) {
        let length = Vector::length(withdraw_list);
        let i = 1;
        while (i < length) {
            let cur = *Vector::borrow(withdraw_list, i);
            let j = i;
            while (j > 0) {
                j = j - 1;
                if (*Vector::borrow(withdraw_list, j) > cur) {
                    Vector::swap(withdraw_list, j, j + 1);
                } else {
                    break
                };
            };
            i = i + 1;
        };
    }

    /// Calculate the total active stake, and the amount of stake to reach quorum.
    fun calculate_total_stake_and_quorum_threshold(validators: &vector<Validator>): (u64, u64) {
        let total = 0;
        let length = Vector::length(validators);
        let i = 0;
        while (i < length) {
            let v = Vector::borrow(validators, i);
            total = total + Validator::stake_amount(v);
            i = i + 1;
        };
        (total, (total + 1) * 2 / 3)
    }

    /// Process the pending stake changes for each validator.
    fun adjust_stake(validators: &mut vector<Validator>, ctx: &mut TxContext) {
        let length = Vector::length(validators);
        let i = 0;
        while (i < length) {
            let validator = Vector::borrow_mut(validators, i);
            Validator::adjust_stake(validator, ctx);
            i = i + 1;
        }
    }

    #[test_only]
    public(script) fun destroy_for_testing(
        self: ValidatorSet,
    ) {
        let ValidatorSet {
            total_stake: _,
            quorum_stake_threshold: _,
            active_validators,
            pending_validators,
            pending_removals: _,
        } = self;
        while (!Vector::is_empty(&active_validators)) {
            let v = Vector::pop_back(&mut active_validators);
            Validator::destroy(v);
        };
        Vector::destroy_empty(active_validators);
        Vector::destroy_empty(pending_validators);
    }
}
