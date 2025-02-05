// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

#[test_only]
module Sui::TestScenario {
    use Sui::ID::{Self, ID, VersionedID};
    use Sui::TxContext::{Self, TxContext};
    use Std::Option::{Self, Option};
    use Std::Vector;

    /// Attempted an operation that required a concluded transaction, but there are none
    const ENO_CONCLUDED_TRANSACTIONS: u64 = 0;

    /// Requested a transfer or user-defined event on an invalid transaction index
    const EINVALID_TX_INDEX: u64 = 1;

    /// Attempted to return an object to the inventory that was not previously removed from the
    /// inventory during the current transaction. Can happen if the user attempts to call
    /// `return_object` on a locally constructed object rather than one returned from a `TestScenario`
    /// function such as `take_object`.
    const ECANT_RETURN_OBJECT: u64 = 2;

    /// Attempted to retrieve an object of a particular type from the inventory, but it is empty.
    /// Can happen if the user already transferred the object or a previous transaction failed to
    /// transfer the object to the user.
    const EEMPTY_INVENTORY: u64 = 3;

    /// Expected 1 object of this type in the tx sender's inventory, but found >1.
    /// Consider using TestScenario::take_object_by_id to select a specific object
    const EINVENTORY_AMBIGUITY: u64 = 4;

    /// The inventory previously contained an object of this type, but it was removed during the current
    /// transaction.
    const EALREADY_REMOVED_OBJECT: u64 = 5;

    /// Object of given ID cannot be found in the inventory.
    const EOBJECT_ID_NOT_FOUND: u64 = 6;

    /// Found two objects with the same ID in the inventory.
    const EDUPLICATE_OBJCET_ID_FOUND: u64 = 7;

    /// Utility for mocking a multi-transaction Sui execution in a single Move procedure.
    /// A `Scenario` maintains a view of the global object pool built up by the execution.
    /// These objects can be accessed via functions like `take_object`, which gives the
    /// transaction sender access to (only) objects in their inventory.
    /// Example usage:
    /// ```
    /// let addr1: address = 0;
    /// let addr2: address = 1;
    /// // begin a test scenario in a context where addr1 is the sender
    /// let scenario = &mut TestScenario::begin(&addr1);
    /// // addr1 sends an object to addr2
    /// {
    ///     let some_object: SomeObject = ... // construct an object
    ///     Transfer::transfer(some_object, copy addr2)
    /// };
    /// // end the first transaction and begin a new one where addr2 is the sender
    /// TestScenario::next_tx(scenario, &addr2)
    /// {
    ///     // remove the SomeObject value from addr2's inventory
    ///     let obj = TestScenario::take_object<SomeObject>(scenario);
    ///     // use it to test some function that needs this value
    ///     SomeObject::some_function(obj)
    /// }
    /// ... // more txes
    /// ```
    struct Scenario has drop {
        ctx: TxContext,
        /// Object ID's that have been removed during the current transaction. Needed to prevent
        /// double removals
        removed: vector<ID>,
        /// The `i`th entry in this vector is the start index for events emitted by the `i`th transaction.
        /// This information allows us to partition events emitted by distinct transactions
        event_start_indexes: vector<u64>,
    }

    /// Begin a new multi-transaction test scenario in a context where `sender` is the tx sender
    public fun begin(sender: &address): Scenario {
        Scenario {
            ctx: TxContext::new_from_address(*sender, 0),
            removed: Vector::empty(),
            event_start_indexes: vector[0],
        }
    }

    /// Advance the scenario to a new transaction where `sender` is the transaction sender
    public fun next_tx(scenario: &mut Scenario, sender: &address) {
        let last_tx_start_index = last_tx_start_index(scenario);
        let old_total_events = last_tx_start_index;

        // Objects that were wrapped during the transaction need to be explicitly handled
        // since there is no dedicated event for object wrapping.
        // We know an object was wrapped if:
        // - it was removed and not returned
        // - it does not appear in an event during the current transaction.
        emit_wrapped_object_events(last_tx_start_index, &scenario.removed);
        // reset `removed` for the next tx
        scenario.removed = Vector::empty();

        // start index for the next tx is the end index for the current one
        let new_total_events = num_events();
        let tx_event_count = new_total_events - old_total_events;
        let event_end_index = last_tx_start_index + tx_event_count;
        Vector::push_back(&mut scenario.event_start_indexes, event_end_index);

        // create a seed for new transaction digest to ensure that this tx has a different
        // digest (and consequently, different object ID's) than the previous tx
        let new_tx_digest_seed = (Vector::length(&scenario.event_start_indexes) as u8);
        scenario.ctx = TxContext::new_from_address(*sender, new_tx_digest_seed);
    }

    /// Remove the object of type `T` from the inventory of the current tx sender in `scenario`.
    /// An object is in the sender's inventory if:
    /// - The object is in the global event log
    /// - The sender owns the object, or the object is immutable
    /// - If the object was previously removed, it was subsequently replaced via a call to `return_object`.
    /// Aborts if there is no object of type `T` in the inventory of the tx sender
    /// Aborts if there is >1 object of type `T` in the inventory of the tx sender--this function
    /// only succeeds when the object to choose is unambiguous. In cases where there are multiple `T`'s,
    /// the caller should resolve the ambiguity by using `take_object_by_id`.
    public fun take_object<T: key>(scenario: &mut Scenario): T {
        let sender = sender(scenario);
        remove_unique_object(scenario, sender)
    }

    /// Remove and return the child object of type `T2` owned by `parent_obj`.
    /// Aborts if there is no object of type `T2` owned by `parent_obj`
    /// Aborts if there is >1 object of type `T2` owned by `parent_obj`--this function
    /// only succeeds when the object to choose is unambiguous. In cases where there are are multiple `T`'s
    /// owned by `parent_obj`, the caller should resolve the ambiguity using `take_nested_object_by_id`.
    public fun take_nested_object<T1: key, T2: key>(
        scenario: &mut Scenario, parent_obj: &T1
    ): T2 {
        remove_unique_object(scenario, ID::id_address(ID::id(parent_obj)))
    }

    /// Same as `take_object`, but returns the object of type `T` with object ID `id`.
    /// Should only be used in cases where current tx sender has more than one object of
    /// type `T` in their inventory.
    public fun take_object_by_id<T: key>(scenario: &mut Scenario, id: ID): T {
        let object_opt: Option<T> = find_object_by_id_in_inventory(scenario, &id);

        assert!(Option::is_some(&object_opt), EOBJECT_ID_NOT_FOUND);
        let object = Option::extract(&mut object_opt);
        Option::destroy_none(object_opt);

        assert!(!Vector::contains(&scenario.removed, &id), EALREADY_REMOVED_OBJECT);
        Vector::push_back(&mut scenario.removed, id);

        object
    }

    /// This function tells you whether calling `take_object_by_id` would succeed.
    /// It provides a way to check without triggering assertions.
    public fun can_take_object_by_id<T: key>(scenario: &Scenario, id: ID): bool {
        let object_opt: Option<T> = find_object_by_id_in_inventory(scenario, &id);
        if (Option::is_none(&object_opt)) {
            Option::destroy_none(object_opt);
            return false
        };
        let object = Option::extract(&mut object_opt);
        Option::destroy_none(object_opt);
        delete_object_for_testing(object);

        return !Vector::contains(&scenario.removed, &id)
    }

    /// Same as `take_nested_object`, but returns the child object of type `T` with object ID `id`.
    /// Should only be used in cases where the parent object has more than one child of type `T`.
    public fun take_nested_object_by_id<T1: key, T2: key>(
        _scenario: &mut Scenario, _parent_obj: &T1, _child_id: ID
    ): T2 {
        // TODO: implement me
        abort(200)
    }

    /// Return `t` to the global object pool maintained by `scenario`.
    /// Subsequent calls to `take_object<T>` will succeed if the object is in the inventory of the current
    /// transaction sender.
    /// Aborts if `t` was not previously taken from the inventory via a call to `take_object` or similar.
    public fun return_object<T: key>(scenario: &mut Scenario, t: T) {
        let id = ID::id(&t);
        let removed = &mut scenario.removed;
        // TODO: add Vector::remove_element to Std that does this 3-liner
        let (is_mem, idx) = Vector::index_of(removed, id);
        // can't return an object we haven't removed
        assert!(is_mem, ECANT_RETURN_OBJECT);
        Vector::remove(removed, idx);

        // Update the object content in the inventory.
        // Because the events are the source of truth for all object values in the inventory,
        // we must put any state change future txes want to see in an event. It would not be safe
        // to do (e.g.) `delete_object_for_testing(t)` instead.
        update_object(t)
    }

    /// Return `true` if a call to `take_object<T>(scenario)` will succeed
    public fun can_take_object<T: key>(scenario: &Scenario): bool {
        let objects: vector<T> = get_inventory<T>(
            sender(scenario),
            last_tx_start_index(scenario)
        );
        let res = !Vector::is_empty(&objects);
        delete_object_for_testing(objects);
        res
    }

    /// Return the `TxContext` associated with this `scenario`
    public fun ctx(scenario: &mut Scenario): &mut TxContext {
        &mut scenario.ctx
    }

    /// Generate a fresh ID for the current tx associated with this `scenario`
    public fun new_id(scenario: &mut Scenario): VersionedID {
        TxContext::new_id(&mut scenario.ctx)
    }

    /// Return the sender of the current tx in this `scenario`
    public fun sender(scenario: &Scenario): address {
        TxContext::sender(&scenario.ctx)
    }

    /// Return the number of concluded transactions in this scenario.
    /// This does not include the current transaction--e.g., this will return 0 if `next_tx` has never been called
    public fun num_concluded_txes(scenario: &Scenario): u64 {
        Vector::length(&scenario.event_start_indexes) - 1
    }

    /// Return the index in the global transaction log where the events emitted by the `tx_idx`th transaction begin
    fun tx_start_index(scenario: &Scenario, tx_idx: u64): u64 {
        let idxs = &scenario.event_start_indexes;
        let len = Vector::length(idxs);
        assert!(tx_idx < len, EINVALID_TX_INDEX);
        *Vector::borrow(idxs, tx_idx)
    }

    /// Return the tx start index of the current transaction. This is an index into the global event log
    /// such that all events emitted by the current transaction occur at or after this index
    fun last_tx_start_index(scenario: &Scenario): u64 {
        let idxs = &scenario.event_start_indexes;
        // Safe because because `event_start_indexes` is always non-empty
        *Vector::borrow(idxs, Vector::length(idxs) - 1)
    }

    /// Remove and return the unique object of type `T` that can be accessed by `signer_address`
    /// Aborts if there are no objects of type `T` that can be be accessed by `signer_address`
    /// Aborts if there is >1 object of type `T` that can be accessed by `signer_address`
    fun remove_unique_object<T: key>(scenario: &mut Scenario, signer_address: address): T {
        let num_concluded_txes = num_concluded_txes(scenario);
        // Can't remove objects transferred by previous transactions if there are none
        assert!(num_concluded_txes != 0, ENO_CONCLUDED_TRANSACTIONS);

        let objects: vector<T> = get_inventory<T>(
            signer_address,
            last_tx_start_index(scenario)
        );
        let objects_len = Vector::length(&objects);
        if (objects_len == 1) {
            // found a unique object. ensure that it hasn't already been removed, then return it
            let t = Vector::pop_back(&mut objects);
            let id = ID::id(&t);
            Vector::destroy_empty(objects);

            assert!(!Vector::contains(&scenario.removed, id), EALREADY_REMOVED_OBJECT);
            Vector::push_back(&mut scenario.removed, *id);
            t
        } else if (objects_len == 0) {
            abort(EEMPTY_INVENTORY)
        } else { // objects_len > 1
            abort(EINVENTORY_AMBIGUITY)
        }
    }

    fun find_object_by_id_in_inventory<T: key>(scenario: &Scenario, id: &ID): Option<T> {
        let sender = sender(scenario);
        let objects: vector<T> = get_inventory<T>(
            sender,
            last_tx_start_index(scenario)
        );
        let object_opt = Option::none();
        while (!Vector::is_empty(&objects)) {
            let element = Vector::pop_back(&mut objects);
            if (ID::id(&element) == id) {
                // Within the same test scenario, there is no way to
                // create two objects with the same ID. So this should
                // be unique.
                Option::fill(&mut object_opt, element);
            } else {
                delete_object_for_testing(element);
            }
        };
        Vector::destroy_empty(objects);

        object_opt
    }

    // TODO: Add API's for inspecting user events, printing the user's inventory, ...

    // ---Natives---

    /// Return all live objects of type `T` that can be accessed by `signer_address` in the current transaction
    /// Events at or beyond `tx_end_index` in the log should not be processed to build this inventory
    native fun get_inventory<T: key>(signer_address: address, tx_end_index: u64): vector<T>;

    /// Test-only function for discarding an arbitrary object.
    /// Useful for eliminating objects without the `drop` ability.
    /// TODO: Rename this function to avoid confusion.
    native fun delete_object_for_testing<T>(t: T);

    /// Return the total number of events emitted by all txes in the current VM execution, including both user-defined events and system events
    native fun num_events(): u64;

    /// Find out all objects that were wrapped during the transaction, and emit an event for each of them.
    native fun emit_wrapped_object_events<ID>(tx_begin_idx: u64, removed: &vector<ID>);

    /// Update the content of an object in the inventory.
    native fun update_object<T: key>(obj: T);
}
