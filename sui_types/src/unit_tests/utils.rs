// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use crate::{crypto::{get_key_pair, KeyPair}, committee::Committee};
use std::collections::BTreeMap;

pub fn make_committee_key() -> (Vec<KeyPair>, Committee) {
    let mut authorities = BTreeMap::new();
    let mut keys = Vec::new();

    for _ in 0..4 {
        let (_, inner_authority_key) = get_key_pair();
        authorities.insert(
            /* address */ *inner_authority_key.public_key_bytes(),
            /* voting right */ 1,
        );
        keys.push(inner_authority_key);
    }

    let committee = Committee::new(authorities);
    (keys, committee)
}