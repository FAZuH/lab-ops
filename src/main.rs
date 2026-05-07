use std::process;

use clap::Parser as _;
use lab_ops::cli::Cli;
use lab_ops::cli::Command;
use lab_ops::cmd::cf2ansible;

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Command::Cf2Ansible {
            zone_file,
            zone_name,
        } => cf2ansible::run(zone_file, zone_name),
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        process::exit(1);
    }
}
