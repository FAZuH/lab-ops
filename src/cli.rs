use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;

pub const CMD_CF2ANSIBLE: &str = "cf2ansible";
pub const CMD_DOCKERNET: &str = "dockernet";
pub const CMD_DOCKERNATMAP: &str = "dockernatmap";

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
    /// dockernatmap daemon management
    #[command(name = CMD_DOCKERNATMAP)]
    DockerNatMap {
        #[command(flatten)]
        args: dockernatmap::cli::Cli,
    },
}
