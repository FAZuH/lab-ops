use std::path::PathBuf;

use clap::Parser;
use clap::Subcommand;

use crate::consts::CMD_AUTO_DISCOVER;
use crate::consts::CMD_CF2ANSIBLE;
use crate::consts::CMD_CF2TERRA;
use crate::consts::CMD_DOCKERNET;
use crate::consts::CMD_NATMAP;

/// Top-level CLI argument parser for `lab-ops`.
#[derive(Parser)]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(name = crate::consts::CMD, about = "Lab operations toolkit")]
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
    /// Converts BIND DNS zone files to Terraform Cloudflare DNS resources.
    #[command(name = CMD_CF2TERRA)]
    Cf2Terra {
        /// Path to the DNS zone file to parse.
        zone_file: PathBuf,
        /// Zone name override; defaults to the name extracted from the SOA record.
        zone_name: Option<String>,
        /// Terraform variable for the Cloudflare Zone ID.
        #[arg(long, default_value = "var.cloudflare_zone_id")]
        zone_id_var: String,
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
    /// Service discovery daemon: watches Docker events, manages port forwarding,
    /// registers services with Consul, and generates nginx configs.
    #[command(name = CMD_AUTO_DISCOVER)]
    AutoDiscover {
        #[command(flatten)]
        args: auto_discover::cli::Cli,
    },
}
