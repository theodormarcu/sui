// Copyright (c) 2022, Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use async_trait::async_trait;
use clap::*;
use colored::Colorize;
use std::{
    io::{stderr, stdout, Write},
    ops::Deref,
    path::PathBuf,
};
use sui::{
    shell::{
        install_shell_plugins, AsyncHandler, CacheKey, CommandStructure, CompletionCache, Shell,
    },
    sui_config_dir,
    wallet_commands::*,
    SUI_WALLET_CONFIG,
};
use tracing::debug;

const SUI: &str = "   _____       _    _       __      ____     __
  / ___/__  __(_)  | |     / /___ _/ / /__  / /_
  \\__ \\/ / / / /   | | /| / / __ `/ / / _ \\/ __/
 ___/ / /_/ / /    | |/ |/ / /_/ / / /  __/ /_
/____/\\__,_/_/     |__/|__/\\__,_/_/_/\\___/\\__/";

#[derive(Parser)]
#[clap(
    name = "wallet",
    about = "A Byzantine fault tolerant chain with low-latency finality and high throughput",
    rename_all = "kebab-case"
)]
struct ClientOpt {
    #[clap(short, global = true)]
    /// Start interactive wallet
    interactive: bool,
    /// Sets the file storing the state of our user accounts (an empty one will be created if missing)
    #[clap(long)]
    config: Option<PathBuf>,
    /// Subcommands. Acceptable values are transfer, query_objects, benchmark, and create_accounts.
    #[clap(subcommand)]
    cmd: Option<WalletCommands>,
    /// Return command outputs in json format.
    #[clap(long, global = true)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let config = telemetry_subscribers::TelemetryConfig {
        service_name: "wallet".into(),
        enable_tracing: std::env::var("SUI_TRACING_ENABLE").is_ok(),
        json_log_output: std::env::var("SUI_JSON_SPAN_LOGS").is_ok(),
        log_file: Some("wallet.log".into()),
        ..Default::default()
    };
    #[allow(unused)]
    let guard = telemetry_subscribers::init(config);
    if let Ok(git_rev) = std::env::var("GIT_REV") {
        debug!("Wallet built at git revision {git_rev}");
    }

    let mut app: Command = ClientOpt::command();
    app = app.no_binary_name(false);
    let options: ClientOpt = ClientOpt::from_arg_matches(&app.get_matches()).unwrap();
    let wallet_conf_path = options
        .config
        .clone()
        .unwrap_or(sui_config_dir()?.join(SUI_WALLET_CONFIG));

    let mut context = WalletContext::new(&wallet_conf_path)?;

    // Sync all accounts on start up.
    for address in context.config.accounts.clone() {
        WalletCommands::SyncClientState {
            address: Some(address),
        }
        .execute(&mut context)
        .await?;
    }

    let mut out = stdout();

    if options.interactive {
        let app: Command = WalletCommands::command();
        writeln!(out, "{}", SUI.cyan().bold())?;
        let mut version = app
            .get_long_version()
            .unwrap_or_else(|| app.get_version().unwrap_or("unknown"))
            .to_owned();
        if let Ok(git_rev) = std::env::var("GIT_REV") {
            version.push('-');
            version.push_str(&git_rev);
        }
        writeln!(out, "--- sui wallet {version} ---")?;
        writeln!(out)?;
        writeln!(out, "{}", context.config.deref())?;
        writeln!(out, "Welcome to the Sui interactive shell.")?;
        writeln!(out)?;

        let mut shell = Shell::new(
            "sui>-$ ".bold().green(),
            context,
            ClientCommandHandler,
            CommandStructure::from_clap(&install_shell_plugins(app)),
        );

        shell.run_async(&mut out, &mut stderr()).await?;
    } else if let Some(mut cmd) = options.cmd {
        cmd.execute(&mut context).await?.print(!options.json);
    } else {
        ClientOpt::command().print_long_help()?
    }
    Ok(())
}

struct ClientCommandHandler;

#[async_trait]
impl AsyncHandler<WalletContext> for ClientCommandHandler {
    async fn handle_async(
        &self,
        args: Vec<String>,
        context: &mut WalletContext,
        completion_cache: CompletionCache,
    ) -> bool {
        if let Err(e) = handle_command(get_command(args), context, completion_cache).await {
            let _err = writeln!(stderr(), "{}", e.to_string().red());
        }
        false
    }
}

fn get_command(args: Vec<String>) -> Result<WalletOpts, anyhow::Error> {
    let app: Command = install_shell_plugins(WalletOpts::command());
    Ok(WalletOpts::from_arg_matches(
        &app.try_get_matches_from(args)?,
    )?)
}

async fn handle_command(
    wallet_opts: Result<WalletOpts, anyhow::Error>,
    context: &mut WalletContext,
    completion_cache: CompletionCache,
) -> Result<(), anyhow::Error> {
    let mut wallet_opts = wallet_opts?;
    let result = wallet_opts.command.execute(context).await?;

    // Update completion cache
    // TODO: Completion data are keyed by strings, are there ways to make it more error proof?
    if let Ok(mut cache) = completion_cache.write() {
        match result {
            WalletCommandResult::Addresses(ref addresses) => {
                let addresses = addresses
                    .iter()
                    .map(|addr| format!("{addr}"))
                    .collect::<Vec<_>>();
                cache.insert(CacheKey::flag("--address"), addresses.clone());
                cache.insert(CacheKey::flag("--to"), addresses);
            }
            WalletCommandResult::Objects(ref objects) => {
                let objects = objects
                    .iter()
                    .map(|(object_id, _, _)| format!("{object_id}"))
                    .collect::<Vec<_>>();
                cache.insert(CacheKey::new("object", "--id"), objects.clone());
                cache.insert(CacheKey::flag("--gas"), objects.clone());
                cache.insert(CacheKey::flag("--object-id"), objects);
            }
            _ => {}
        }
    }
    result.print(!wallet_opts.json);
    Ok(())
}
