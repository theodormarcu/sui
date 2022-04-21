// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

#[test_only]
module Sui::ValidatorSetTests {
    use Sui::Coin;
    //use Sui::ID;
    //use Sui::SUI::SUI;
    use Sui::TxContext::{Self, TxContext};
    use Sui::Validator::{Self, Validator};
    use Sui::ValidatorSet;

    #[test]
    public(script) fun test_validator_set_flow() {
        // Create 4 validators, with stake 100, 200, 300, 400.
        let (ctx1, validator1) = create_validator(@0x1, 1);
        let (ctx2, validator2) = create_validator(@0x2, 2);
        let (_ctx3, validator3) = create_validator(@0x3, 3);
        let (_ctx4, validator4) = create_validator(@0x4, 4);

        // Create a validator set with only the first validator in it.
        let validator_set = ValidatorSet::new(vector[validator1]);
        assert!(ValidatorSet::get_total_validator_candidate_count(&validator_set) == 1, 0);
        assert!(ValidatorSet::get_total_stake(&validator_set) == 100, 0);
        assert!(ValidatorSet::get_quorum_threshold_pct(&validator_set) == 100, 0);

        // Add the other 3 validators one by one.
        ValidatorSet::request_add_validator(
            &mut validator_set,
            validator2,
        );
        // Adding validator during the epoch should not affect stake and quorum threshold.
        assert!(ValidatorSet::get_total_validator_candidate_count(&validator_set) == 2, 0);
        assert!(ValidatorSet::get_total_stake(&validator_set) == 100, 0);
        assert!(ValidatorSet::get_quorum_threshold_pct(&validator_set) == 100, 0);

        ValidatorSet::request_add_validator(
            &mut validator_set,
            validator3,
        );
        ValidatorSet::request_add_stake(
            &mut validator_set,
            Coin::mint_for_testing(500, &mut ctx1),
            600 /* max_validator_stake */,
            &ctx1,
        );
        // Adding stake to existing active validator during the epoch
        // should not change total stake.
        assert!(ValidatorSet::get_total_stake(&validator_set) == 100, 0);
        ValidatorSet::request_add_stake(
            &mut validator_set,
            Coin::mint_for_testing(600, &mut ctx2),
            800 /* max_validator_stake */,
            &ctx2,
        );
        // Adding stake to pending validator does not change total stake.
        assert!(ValidatorSet::get_total_stake(&validator_set) == 100, 0);

        ValidatorSet::request_withdraw_stake(
            &mut validator_set,
            500,
            100 /* min_validator_stake */,
            &ctx1,
        );
        assert!(ValidatorSet::get_total_stake(&validator_set) == 100, 0);

        ValidatorSet::request_add_validator(
            &mut validator_set,
            validator4,
        );

        ValidatorSet::advance_epoch(&mut validator_set, &mut ctx1);
        // The total stake and quorum should reflect 4 validators.
        assert!(ValidatorSet::get_total_validator_candidate_count(&validator_set) == 4, 0);
        assert!(ValidatorSet::get_total_stake(&validator_set) == 1600, 0);
        assert!(ValidatorSet::get_quorum_threshold_pct(&validator_set) == 75, 0);

        ValidatorSet::request_remove_validator(
            &mut validator_set,
            &ctx1,
        );
        // Total validator candidate count changes, but total stake remains during epoch.
        assert!(ValidatorSet::get_total_validator_candidate_count(&validator_set) == 3, 0);
        assert!(ValidatorSet::get_total_stake(&validator_set) == 1600, 0);
        ValidatorSet::advance_epoch(&mut validator_set, &mut ctx1);
        // Validator1 is gone.
        assert!(ValidatorSet::get_total_stake(&validator_set) == 1500, 0);
        assert!(ValidatorSet::get_quorum_threshold_pct(&validator_set) == 100, 0);

        ValidatorSet::destroy_for_testing(validator_set);
    }

    fun create_validator(addr: address, hint: u8): (TxContext, Validator) {
        let stake_value = (hint as u64) * 100;
        let ctx = TxContext::new_from_address(addr, hint);
        let init_stake = Coin::mint_for_testing(stake_value, &mut ctx);
        let validator = Validator::new(
            addr,
            vector[hint],
            vector[hint],
            init_stake,
            &mut ctx,
        );
        (ctx, validator)
    }
}