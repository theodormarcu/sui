// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use move_binary_format::CompiledModule;
use once_cell::sync::Lazy;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use sui_framework::{self, DEFAULT_FRAMEWORK_PATH};
use sui_types::base_types::{ObjectRef, SuiAddress, TxContext};
use sui_types::error::SuiResult;
use sui_types::SUI_FRAMEWORK_ADDRESS;
use sui_types::{base_types::TransactionDigest, object::Object};

static GENESIS: Lazy<Mutex<Genesis>> = Lazy::new(|| {
    Mutex::new(create_genesis_module_objects(&PathBuf::from(DEFAULT_FRAMEWORK_PATH)).unwrap())
});

struct Genesis {
    pub objects: Vec<Object>,
    pub modules: Vec<Vec<CompiledModule>>,
}

pub fn clone_genesis_compiled_modules() -> Vec<Vec<CompiledModule>> {
    let genesis = GENESIS.lock().unwrap();
    genesis.modules.clone()
}

pub fn clone_genesis_packages() -> Vec<Object> {
    let genesis = GENESIS.lock().unwrap();
    genesis.objects.clone()
}

pub fn get_framework_object_ref() -> ObjectRef {
    let genesis = GENESIS.lock().unwrap();
    genesis
        .objects
        .iter()
        .find(|o| o.id() == SUI_FRAMEWORK_ADDRESS.into())
        .unwrap()
        .compute_object_reference()
}

pub fn get_genesis_context() -> TxContext {
    get_genesis_context_with_custom_address(&SuiAddress::default())
}

pub fn get_genesis_context_with_custom_address(address: &SuiAddress) -> TxContext {
    TxContext::new(address, &TransactionDigest::genesis())
}

/// Create and return objects wrapping the genesis modules for sui
fn create_genesis_module_objects(lib_dir: &Path) -> SuiResult<Genesis> {
    let sui_modules = sui_framework::get_sui_framework_modules(lib_dir)?;
    let std_modules =
        sui_framework::get_move_stdlib_modules(&lib_dir.join("deps").join("move-stdlib"))?;
    let objects = vec![
        Object::new_package(std_modules.clone(), TransactionDigest::genesis()),
        Object::new_package(sui_modules.clone(), TransactionDigest::genesis()),
    ];
    let modules = vec![std_modules, sui_modules];
    Ok(Genesis { objects, modules })
}
