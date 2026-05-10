use clap::Parser as _;
use color_eyre::Result;
use lab_ops::cli::Cli;
use lab_ops::cli::Command;
use lab_ops::cmd::cf2ansible;
use lab_ops::cmd::dockernet;

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    match cli.command {
        Command::Cf2Ansible {
            zone_file,
            zone_name,
        } => cf2ansible::run(zone_file, zone_name)?,
        Command::DockerNet => {
            use tokio::runtime::Builder;

            let rt = Builder::new_current_thread().enable_all().build()?;
            rt.block_on(dockernet::run())?;
        }
        Command::NatMap { args } => {
            use tokio::runtime::Builder;
            let rt = Builder::new_current_thread().enable_all().build()?;
            rt.block_on(natmap::cli::run_cli_with_args(args))?;
        }
    };

    Ok(())
}
