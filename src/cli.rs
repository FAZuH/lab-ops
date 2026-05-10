use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;

pub const CMD_CF2ANSIBLE: &str = "cf2ansible";
pub const CMD_DOCKERNET: &str = "dockernet";
pub const CMD_NATMAP: &str = "natmap";

#[derive(Parser)]
#[command(name = "lab-ops", about = "Lab operations toolkit")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Convert BIND DNS zone files to Ansible YAML tasks
    #[command(name = CMD_CF2ANSIBLE)]
    Cf2Ansible {
        /// Path to the DNS zone file
        zone_file: PathBuf,
        /// Zone name (defaults to name extracted from SOA record)
        zone_name: Option<String>,
    },
    /// View addresses and binds of Docker containers
    #[command(name = CMD_DOCKERNET)]
    DockerNet,
    /// Manage iptables NAT rules (static VMs & dynamic Docker)
    #[command(name = CMD_NATMAP)]
    NatMap {
        #[command(flatten)]
        args: natmap::cli::Cli,
    },
}
