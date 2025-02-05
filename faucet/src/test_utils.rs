// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use sui::{
    sui_commands::SuiNetwork,
    wallet_commands::{WalletCommands, WalletContext},
};
use sui_types::base_types::SuiAddress;

use sui::config::{AuthorityPrivateInfo, Config, GenesisConfig, WalletConfig};
use sui::gateway_config::{GatewayConfig, GatewayType};
use sui::keystore::KeystoreType;
use sui::sui_commands::genesis;
use sui::{SUI_GATEWAY_CONFIG, SUI_NETWORK_CONFIG, SUI_WALLET_CONFIG};

/* -------------------------------------------------------------------------- */
/*  NOTE: Below is copied from sui/src/unit_tests, we should consolidate them */
/* -------------------------------------------------------------------------- */

pub async fn setup_network_and_wallet(
) -> Result<(SuiNetwork, WalletContext, SuiAddress), anyhow::Error> {
    let working_dir = tempfile::tempdir()?;

    let network = start_test_network(working_dir.path(), None).await?;

    // Create Wallet context.
    let wallet_conf = working_dir.path().join(SUI_WALLET_CONFIG);
    let mut context = WalletContext::new(&wallet_conf)?;
    let address = context.config.accounts.first().cloned().unwrap();

    // Sync client to retrieve objects from the network.
    WalletCommands::SyncClientState {
        address: Some(address),
    }
    .execute(&mut context)
    .await?;
    Ok((network, context, address))
}

pub async fn start_test_network(
    working_dir: &Path,
    genesis_config: Option<GenesisConfig>,
) -> Result<SuiNetwork, anyhow::Error> {
    let working_dir = working_dir.to_path_buf();
    let network_path = working_dir.join(SUI_NETWORK_CONFIG);
    let wallet_path = working_dir.join(SUI_WALLET_CONFIG);
    let keystore_path = working_dir.join("wallet.key");
    let db_folder_path = working_dir.join("client_db");

    let mut genesis_config =
        genesis_config.unwrap_or(GenesisConfig::default_genesis(&working_dir)?);
    let authorities = genesis_config
        .authorities
        .iter()
        .map(|info| AuthorityPrivateInfo {
            key_pair: info.key_pair.copy(),
            host: info.host.clone(),
            port: 0,
            db_path: info.db_path.clone(),
            stake: info.stake,
            consensus_address: info.consensus_address,
            address: SuiAddress::from(info.key_pair.public_key_bytes()),
        })
        .collect();
    genesis_config.authorities = authorities;

    let (network_config, accounts, mut keystore) = genesis(genesis_config).await?;
    let network = SuiNetwork::start(&network_config).await?;

    let network_config = network_config.persisted(&network_path);
    network_config.save()?;
    keystore.set_path(&keystore_path);
    keystore.save()?;

    let authorities = network_config.get_authority_infos();
    let authorities = authorities
        .into_iter()
        .zip(&network.spawned_authorities)
        .map(|(mut info, server)| {
            info.base_port = server.get_port();
            info
        })
        .collect::<Vec<_>>();
    let active_address = accounts.get(0).copied();

    GatewayConfig {
        db_folder_path: db_folder_path.clone(),
        authorities: authorities.clone(),
        ..Default::default()
    }
    .persisted(&working_dir.join(SUI_GATEWAY_CONFIG))
    .save()?;

    // Create wallet config with stated authorities port
    WalletConfig {
        accounts,
        keystore: KeystoreType::File(keystore_path),
        gateway: GatewayType::Embedded(GatewayConfig {
            db_folder_path,
            authorities,
            ..Default::default()
        }),
        active_address,
    }
    .persisted(&wallet_path)
    .save()?;

    // Return network handle
    Ok(network)
}
