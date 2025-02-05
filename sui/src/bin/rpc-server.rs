// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use clap::Parser;
use jsonrpsee::{
    http_server::{AccessControlBuilder, HttpServerBuilder},
    RpcModule,
};
use std::{
    env,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};
use sui::{
    rpc_gateway::{RpcGatewayImpl, RpcGatewayServer},
    sui_config_dir,
};
use tracing::info;

const DEFAULT_RPC_SERVER_PORT: &str = "5001";
const DEFAULT_RPC_SERVER_ADDR_IPV4: &str = "127.0.0.1";

#[cfg(test)]
#[path = "../unit_tests/rpc_server_tests.rs"]
mod rpc_server_tests;

#[derive(Parser)]
#[clap(
    name = "Sui RPC Gateway",
    about = "A Byzantine fault tolerant chain with low-latency finality and high throughput",
    rename_all = "kebab-case"
)]
struct RpcGatewayOpt {
    #[clap(long)]
    config: Option<PathBuf>,

    #[clap(long, default_value = DEFAULT_RPC_SERVER_PORT)]
    port: u16,

    #[clap(long, default_value = DEFAULT_RPC_SERVER_ADDR_IPV4)]
    host: Ipv4Addr,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let config = telemetry_subscribers::TelemetryConfig {
        service_name: "rpc_gateway".into(),
        enable_tracing: std::env::var("SUI_TRACING_ENABLE").is_ok(),
        json_log_output: std::env::var("SUI_JSON_SPAN_LOGS").is_ok(),
        ..Default::default()
    };
    #[allow(unused)]
    let guard = telemetry_subscribers::init(config);
    let options: RpcGatewayOpt = RpcGatewayOpt::parse();
    let config_path = options
        .config
        .unwrap_or(sui_config_dir()?.join("gateway.conf"));
    info!("Gateway config file path: {:?}", config_path);

    let server_builder = HttpServerBuilder::default();
    let mut ac_builder = AccessControlBuilder::default();

    if let Ok(value) = env::var("ACCESS_CONTROL_ALLOW_ORIGIN") {
        let list = value.split(',').collect::<Vec<_>>();
        info!("Setting ACCESS_CONTROL_ALLOW_ORIGIN to : {:?}", list);
        ac_builder = ac_builder.set_allowed_origins(list)?;
    }

    let server = server_builder
        .set_access_control(ac_builder.build())
        .build(SocketAddr::new(IpAddr::V4(options.host), options.port))
        .await?;

    let mut module = RpcModule::new(());
    module.merge(RpcGatewayImpl::new(&config_path)?.into_rpc())?;

    info!(
        "Available JSON-RPC methods : {:?}",
        module.method_names().collect::<Vec<_>>()
    );

    let addr = server.local_addr()?;
    let server_handle = server.start(module)?;
    info!("Sui RPC Gateway listening on local_addr:{}", addr);

    server_handle.await;
    Ok(())
}
