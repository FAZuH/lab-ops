use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;

/// Subcommand name for the DNS zone-to-Ansible converter.
pub const CMD_CF2ANSIBLE: &str = "cf2ansible";
/// Subcommand name for the Docker network viewer.
pub const CMD_DOCKERNET: &str = "dockernet";
/// Subcommand name for the NAT mapping tool.
pub const CMD_NATMAP: &str = "natmap";

/// Top-level CLI argument parser for `lab-ops`.
#[derive(Parser)]
#[command(name = "lab-ops", about = "Lab operations toolkit")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// All supported subcommands of lab-ops.
#[derive(Subcommand)]
pub enum Command {
    /// Converts BIND DNS zone files to Ansible YAML tasks.
    #[command(name = CMD_CF2ANSIBLE)]
    Cf2Ansible {
        /// Path to the DNS zone file to parse.
        zone_file: PathBuf,
        /// Zone name override; defaults to the name extracted from the SOA record.
        zone_name: Option<String>,
    },
    /// Displays IP addresses and port bindings of Docker containers.
    #[command(name = CMD_DOCKERNET)]
    DockerNet,
    /// Manages iptables NAT rules for static VMs and dynamic Docker containers.
    #[command(name = CMD_NATMAP)]
    NatMap {
        #[command(flatten)]
        args: natmap::cli::Cli,
    },
}
