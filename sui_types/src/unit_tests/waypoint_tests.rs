// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use super::*;
use rand::Rng;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Item([u8; 8]);

impl Borrow<[u8]> for Item {
    fn borrow(&self) -> &[u8] {
        &self.0[..]
    }
}

impl Borrow<[u8]> for &Item {
    fn borrow(&self) -> &[u8] {
        &self.0[..]
    }
}

fn make_item() -> Item {
    let mut rng = rand::thread_rng();
    let item: [u8; 8] = rng.gen();
    Item(item)
}

#[test]
fn test_diff() {
    let mut first = Waypoint::new(10);
    let mut second = Waypoint::new(10);

    let v1 = make_item();
    let v2 = make_item();
    let v3 = make_item();
    let v4 = make_item();

    first.insert(&v1);
    first.insert(&v2);
    first.insert(&v3);

    second.insert(&v1);
    second.insert(&v2);
    second.insert(&v4);

    let diff = WaypointDiff::new(
        0,
        first,
        vec![v4].into_iter(),
        1,
        second,
        vec![v3].into_iter(),
    );
    assert!(diff.check());
}

#[test]
fn test_checkpoint() {
    let mut w1 = Waypoint::new(10);
    let mut w2 = Waypoint::new(10);
    let mut w3 = Waypoint::new(10);

    let v1 = make_item();
    let v2 = make_item();
    let v3 = make_item();
    let v4 = make_item();

    w1.insert(&v1);
    w1.insert(&v2);

    w2.insert(&v1);
    w2.insert(&v3);

    w3.insert(&v2);
    w3.insert(&v3);
    w3.insert(&v4);

    let diff1 = WaypointDiff::new(
        0,
        w1.clone(),
        vec![v3.clone()].into_iter(),
        1,
        w2.clone(),
        vec![v2.clone()].into_iter(),
    );
    assert!(diff1.check());

    let diff2 = WaypointDiff::new(
        0,
        w1.clone(),
        vec![v3.clone(), v4.clone()].into_iter(),
        2,
        w3.clone(),
        vec![v1.clone()].into_iter(),
    );
    assert!(diff2.check());

    let mut ck = GlobalCheckpoint::new(10);
    assert!(ck.insert(diff1).is_ok());
    assert!(ck.check());
    assert!(ck.insert(diff2).is_ok());
    assert!(ck.check());

    // Now test catch_up_items
    let mut w4 = Waypoint::new(10);
    let v5 = make_item();

    w4.insert(&v5);
    w4.insert(&v1);

    let diff3 = WaypointDiff::new(
        3,
        w4.clone(),
        vec![v2.clone()].into_iter(),
        0,
        w1.clone(),
        vec![v5].into_iter(),
    );
    assert!(diff3.check());

    assert!(ck.catch_up_items(diff3).is_ok());
}
