// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

/// Example of objects that can be combined to create
/// new objects
module Basics::Sandwich {
    use Sui::Coin::{Self, Coin};
    use Sui::ID::{Self, VersionedID};
    use Sui::SUI::SUI;
    use Sui::Transfer;
    use Sui::TxContext::{Self, TxContext};

    struct Ham has key {
        id: VersionedID
    }

    struct Bread has key {
        id: VersionedID
    }

    struct Sandwich has key {
        id: VersionedID,
    }

    // This Capability allows the owner to withdraw profits
    struct GroceryOwnerCapability has key {
        id: VersionedID
    }

    // Grocery is created on module init
    struct Grocery has key {
        id: VersionedID,
        profits: Coin<SUI>
    }

    /// Price for ham
    const HAM_PRICE: u64 = 10;
    /// Price for bread
    const BREAD_PRICE: u64 = 2;

    /// Not enough funds to pay for the good in question
    const EINSUFFICIENT_FUNDS: u64 = 0;

    /// Nothing to withdraw
    const ENO_PROFITS: u64 = 1;

    /// On module init, create a grocery
    fun init(ctx: &mut TxContext) {
        Transfer::share_object(Grocery { 
            id: TxContext::new_id(ctx),
            profits: Coin::zero<SUI>(ctx)
        });

        Transfer::transfer(GroceryOwnerCapability {
            id: TxContext::new_id(ctx)
        }, TxContext::sender(ctx));
    }

    /// Exchange `c` for some ham
    public(script) fun buy_ham(
        grocery: &mut Grocery, 
        c: Coin<SUI>, 
        ctx: &mut TxContext
    ) {
        assert!(Coin::value(&c) == HAM_PRICE, EINSUFFICIENT_FUNDS);
        Coin::join(&mut grocery.profits, c);
        Transfer::transfer(Ham { id: TxContext::new_id(ctx) }, TxContext::sender(ctx))
    }

    /// Exchange `c` for some bread
    public(script) fun buy_bread(
        grocery: &mut Grocery, 
        c: Coin<SUI>, 
        ctx: &mut TxContext
    ) {
        assert!(Coin::value(&c) == BREAD_PRICE, EINSUFFICIENT_FUNDS);
        Coin::join(&mut grocery.profits, c);
        Transfer::transfer(Bread { id: TxContext::new_id(ctx) }, TxContext::sender(ctx))
    }

    /// Combine the `ham` and `bread` into a delicious sandwich
    public(script) fun make_sandwich(
        ham: Ham, bread: Bread, ctx: &mut TxContext
    ) {
        let Ham { id: ham_id } = ham;
        let Bread { id: bread_id } = bread;
        ID::delete(ham_id);
        ID::delete(bread_id);
        Transfer::transfer(Sandwich { id: TxContext::new_id(ctx) }, TxContext::sender(ctx))
    }

    /// See the profits of a grocery
    public fun profits(grocery: &Grocery): u64 {
        Coin::value(&grocery.profits)
    }

    /// Owner of the grocery can collect profits by passing his capability
    public(script) fun collect_profits(_cap: &GroceryOwnerCapability, grocery: &mut Grocery, ctx: &mut TxContext) {
        let amount = Coin::value(&grocery.profits);
        
        assert!(amount > 0, ENO_PROFITS);

        let coin = Coin::withdraw(&mut grocery.profits, amount, ctx);
        Transfer::transfer(coin, TxContext::sender(ctx));
    }

    #[test_only]
    public fun init_for_testing(ctx: &mut TxContext) {
        init(ctx);
    }
}

#[test_only]
module Basics::TestSandwich {
    use Basics::Sandwich::{Self, Grocery, GroceryOwnerCapability, Bread, Ham};
    use Sui::TestScenario;
    use Sui::Coin::{Self};
    use Sui::SUI::SUI;
    
    #[test]
    public(script) fun test_make_sandwich() {
        let owner = @0x1;
        let the_guy = @0x2;

        let scenario = &mut TestScenario::begin(&owner);
        TestScenario::next_tx(scenario, &owner);
        {
            Sandwich::init_for_testing(TestScenario::ctx(scenario));
        };

        TestScenario::next_tx(scenario, &the_guy);
        {
            let grocery = TestScenario::take_object<Grocery>(scenario);
            let ctx = TestScenario::ctx(scenario);
            
            Sandwich::buy_ham(
                &mut grocery, 
                Coin::mint_for_testing<SUI>(10, ctx), 
                ctx
            );
            
            Sandwich::buy_bread(
                &mut grocery, 
                Coin::mint_for_testing<SUI>(2, ctx), 
                ctx
            );
            
            TestScenario::return_object(scenario, grocery);
        };

        TestScenario::next_tx(scenario, &the_guy);
        {
            let grocery = TestScenario::take_object<Grocery>(scenario);
            let ham = TestScenario::take_object<Ham>(scenario);
            let bread = TestScenario::take_object<Bread>(scenario);

            Sandwich::make_sandwich(ham, bread, TestScenario::ctx(scenario));
            TestScenario::return_object(scenario, grocery);
        };

        TestScenario::next_tx(scenario, &owner);
        {  
            let grocery = TestScenario::take_object<Grocery>(scenario);
            let capability = TestScenario::take_object<GroceryOwnerCapability>(scenario);

            assert!(Sandwich::profits(&grocery) == 12, 0);
            Sandwich::collect_profits(&capability, &mut grocery, TestScenario::ctx(scenario));
            assert!(Sandwich::profits(&grocery) == 0, 0);

            TestScenario::return_object(scenario, capability);
            TestScenario::return_object(scenario, grocery);
        };
    }
}
