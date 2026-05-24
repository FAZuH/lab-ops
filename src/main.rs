//! Binary entrypoint for lab-ops.
//!
//! Parses CLI arguments and dispatches to the appropriate subcommand handler.

use clap::Parser;
use color_eyre::Result;
use lab_ops::cli::Cli;
use lab_ops::cli::Command;
use lab_ops::cmd::cf2ansible;
use lab_ops::cmd::cf2terra;
use lab_ops::cmd::dockernet;

fn main() -> Result<()> {
    color_eyre::install()?;

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Command::Cf2Ansible {
            zone_file,
            zone_name,
        } => cf2ansible::run(zone_file, zone_name)?,
        Command::Cf2Terra {
            zone_file,
            zone_name,
            zone_id_var,
        } => cf2terra::run(zone_file, zone_name, zone_id_var)?,
        Command::DockerNet => {
            use tokio::runtime::Builder;

            let rt = Builder::new_current_thread().enable_all().build()?;
            rt.block_on(dockernet::run())?;
        }
        Command::NatMap { args } => {
            use tokio::runtime::Builder;
            let rt = Builder::new_current_thread().enable_all().build()?;
            rt.block_on(natmap::cli::run_cli(args))?;
        }
        Command::AutoDiscover { args } => {
            use tokio::runtime::Builder;
            let rt = Builder::new_current_thread().enable_all().build()?;
            rt.block_on(auto_discover::cli::run_cli(args))?;
        }
    };

    Ok(())
}
