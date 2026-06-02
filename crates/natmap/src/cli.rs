//! CLI argument parsing for the `natmap` subcommands.
//!
//! Defines the [`Cli`] struct and [`NatMapCommand`] / [`DockerCommand`] enums
//! that are flattened into the top-level `lab-ops` CLI via clap.

use std::path::PathBuf;
use std::process::Command;

use clap::Parser;
use clap::Subcommand;
use clap_complete::engine::ArgValueCompleter;
use color_eyre::Result;

use crate::command::add;
use crate::command::handle_clear;
use crate::command::handle_dnat;
use crate::command::handle_hairpin;
use crate::command::handle_list;
use crate::command::handle_policy_route;
use crate::command::handle_snat;
use crate::command::remap;
use crate::command::remove;
use crate::consts::DAEMON_SOCK;
use crate::consts::PKG_NAME;
use crate::consts::STATE;
use crate::daemon::Daemon;

/// CLI arguments for the `natmap` subcommand.
#[derive(Parser, Debug)]
#[command(
    name = PKG_NAME,
    about = "Manage iptables NAT rules (static VMs & dynamic Docker)"
)]
pub struct Cli {
    /// Path to the daemon's Unix socket.
    #[arg(long, default_value = DAEMON_SOCK, global = true)]
    pub socket: PathBuf,

    /// Output results as JSON instead of formatted tables.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: NatMapCommand,
}

/// Top-level natmap subcommands.
#[derive(Subcommand, Debug)]
pub enum NatMapCommand {
    /// Adds or deletes DNAT port forwarding rules.
    #[command(name = "dnat")]
    Dnat {
        /// External (public) IP address.
        #[arg(long)]
        ext_ip: String,
        /// Internal (private) IP address.
        #[arg(long)]
        int_ip: String,
        /// Transport protocol (`tcp` or `udp`).
        #[arg(long, default_value = "tcp")]
        proto: String,
        /// Comma-separated list of ports or port ranges.
        #[arg(long)]
        ports: String,
        /// External network interface (optional).
        #[arg(long)]
        ext_if: Option<String>,
        /// Whether to delete the rule instead of adding it.
        #[arg(long)]
        delete: bool,
        /// Don't masquerade
        #[arg(long)]
        no_masquerade: bool,
    },
    /// Adds or deletes SNAT (masquerade) rules.
    #[command(name = "snat")]
    Snat {
        /// Internal source IP address.
        #[arg(long)]
        int_ip: String,
        /// External network interface.
        #[arg(long)]
        ext_if: String,
        /// External (source NAT) IP address.
        #[arg(long)]
        ext_ip: String,
        /// Whether to delete the rule instead of adding it.
        #[arg(long)]
        delete: bool,
    },
    /// Adds or deletes hairpin NAT rules for internal-to-external access.
    #[command(name = "hairpin")]
    Hairpin {
        /// External IP address.
        #[arg(long)]
        ext_ip: String,
        /// Internal IP address.
        #[arg(long)]
        int_ip: String,
        /// Transport protocol (`tcp` or `udp`).
        #[arg(long, default_value = "tcp")]
        proto: String,
        /// Comma-separated list of ports.
        #[arg(long)]
        ports: String,
        /// LAN source CIDR for MASQUERADE. When set, only traffic from this
        /// subnet is MASQUERADEd (skips PREROUTING DNAT, uses LAN-limited
        /// MASQUERADE). Used for preserve_src_ip hairpin.
        #[arg(long)]
        lan_cidr: Option<String>,
        /// Whether to delete the rule instead of adding it.
        #[arg(long)]
        delete: bool,
    },
    /// Adds or deletes a policy routing rule to send return traffic via a gateway.
    #[command(name = "policy-route")]
    PolicyRoute {
        /// Source IP of this host (packets FROM this IP will use the policy route)
        #[arg(long)]
        src_ip: String,
        /// Gateway IP to route return traffic through
        #[arg(long)]
        via: String,
        /// Routing table ID (default: 100)
        #[arg(long, default_value = "100")]
        table: u32,
        /// Whether to delete the rule instead of adding it
        #[arg(long)]
        delete: bool,
    },
    /// Lists all NAT rules (static iptables + Docker mappings).
    #[command(name = "ls")]
    List {
        /// Optional container ID or name to filter Docker mappings.
        #[arg(
            value_name = "CONTAINER_ID",
            add = ArgValueCompleter::new(crate::completions::complete_container_id)
        )]
        container_id: Option<String>,
    },
    /// Removes all managed NAT rules and resets daemon state.
    #[command(name = "clear")]
    Clear,
    /// Manages Docker container port mappings.
    #[command(name = "docker")]
    Docker {
        #[command(subcommand)]
        cmd: DockerCommand,
    },
    /// Saves current iptables rules to `/etc/iptables/rules.v4`.
    #[command(name = "save")]
    Save,
    /// Enables IP forwarding via `sysctl -w net.ipv4.ip_forward=1`.
    #[command(name = "fwd")]
    Fwd,
    /// Runs the natmap daemon.
    #[command(name = "daemon")]
    Daemon {
        /// Path to the state JSON file.
        #[arg(long, default_value = STATE)]
        state: PathBuf,
        /// Path for the Unix socket.
        #[arg(long, default_value = DAEMON_SOCK)]
        socket: PathBuf,
        /// Unix group for socket access.
        #[arg(long, default_value = PKG_NAME)]
        socket_group: String,
    },
    /// Installs the natmap daemon as a systemd service.
    #[command(name = "install")]
    Install {
        /// Unix group for daemon access.
        #[arg(long, default_value = PKG_NAME)]
        group: String,
        /// Path to the binary to install.
        #[arg(long, default_value = lab_ops_lab_lib::consts::LABOPS_BIN)]
        binary: String,
    },
}

/// Docker-specific subcommands for port mapping management.
#[derive(Subcommand, Debug)]
pub enum DockerCommand {
    /// Adds a new port mapping to a running container.
    #[command(name = "add")]
    Add {
        /// Container ID.
        #[arg(
            value_name = "CONTAINER_ID",
            add = ArgValueCompleter::new(crate::completions::complete_container_id)
        )]
        container_id: String,
        /// Port mapping in the form `[HOST_IP:]HOST_PORT[:[TARGET_IP:]TARGET_PORT][/PROTO]`.
        /// Optional when --name is used (CONTAINER_ID becomes the mapping).
        #[arg(value_name = "MAPPING")]
        mapping: Option<String>,
        /// Container name (alternative to CONTAINER_ID). When set, CONTAINER_ID is treated as the mapping.
        #[arg(
            long,
            add = ArgValueCompleter::new(crate::completions::complete_container_id)
        )]
        name: Option<String>,
    },
    /// Removes one or more Docker port mappings.
    #[command(name = "rm")]
    Remove {
        /// Container ID or name (required unless `--id` or `--name` is used).
        #[arg(
            value_name = "CONTAINER_ID",
            add = ArgValueCompleter::new(crate::completions::complete_container_id)
        )]
        container_id: Option<String>,
        /// Port and optional protocol (e.g., `8080/tcp`).
        #[arg(value_name = "PORT[/PROTO]")]
        port: Option<String>,
        /// Removes all mappings for the specified container.
        #[arg(long)]
        all: bool,
        /// Removes a mapping by its numeric ID.
        #[arg(long)]
        id: Option<u64>,
        /// Container name (alternative to CONTAINER_ID).
        #[arg(
            long,
            add = ArgValueCompleter::new(crate::completions::complete_container_id)
        )]
        name: Option<String>,
    },
    /// Remaps a host port for a running container without restarting it.
    #[command(name = "remap")]
    Remap {
        /// Container ID or name.
        #[arg(
            value_name = "CONTAINER_ID",
            add = ArgValueCompleter::new(crate::completions::complete_container_id)
        )]
        container_id: String,
        /// Port mapping in the form `OLD_PORT:NEW_PORT`.
        #[arg(value_name = "OLD_PORT:NEW_PORT")]
        mapping: String,
    },
}

/// Dispatches a parsed [`Cli`] to the appropriate daemon API call.
pub async fn run_cli(cli: Cli, use_color: bool) -> Result<()> {
    let socket = cli.socket;
    let json = cli.json;

    match cli.command {
        NatMapCommand::Dnat {
            ext_ip,
            int_ip,
            proto,
            ports,
            ext_if,
            delete,
            no_masquerade,
        } => {
            handle_dnat(
                ext_ip,
                int_ip,
                proto,
                ports,
                ext_if,
                delete,
                no_masquerade,
                &socket,
            )
            .await?;
        }
        NatMapCommand::Snat {
            int_ip,
            ext_if,
            ext_ip,
            delete,
        } => {
            handle_snat(int_ip, ext_if, ext_ip, delete, &socket).await?;
        }
        NatMapCommand::Hairpin {
            ext_ip,
            int_ip,
            proto,
            ports,
            lan_cidr,
            delete,
        } => {
            handle_hairpin(ext_ip, int_ip, proto, ports, lan_cidr, delete, &socket).await?;
        }
        NatMapCommand::PolicyRoute {
            src_ip,
            via,
            table,
            delete,
        } => {
            handle_policy_route(src_ip, via, table, delete, &socket).await?;
        }
        NatMapCommand::List { container_id } => {
            handle_list(&socket, container_id, json, use_color).await?;
        }
        NatMapCommand::Clear => {
            handle_clear(&socket).await?;
        }
        NatMapCommand::Docker { cmd } => match cmd {
            DockerCommand::Add {
                container_id,
                mapping,
                name,
            } => {
                add(container_id, mapping, name, &socket, json).await?;
            }
            DockerCommand::Remove {
                container_id,
                port,
                all,
                id,
                name,
            } => {
                remove(container_id, port, all, id, name, &socket, json).await?;
            }
            DockerCommand::Remap {
                container_id,
                mapping,
            } => {
                remap(container_id, mapping, &socket, json).await?;
            }
        },
        NatMapCommand::Save => {
            Command::new("sh")
                .arg("-c")
                .arg("iptables-save > /etc/iptables/rules.v4")
                .status()?;
        }
        NatMapCommand::Fwd => {
            let status = Command::new("sysctl")
                .arg("-w")
                .arg("net.ipv4.ip_forward=1")
                .status()?;
            if !status.success() {
                color_eyre::eyre::bail!("Failed to enable IP forwarding");
            }
        }
        NatMapCommand::Daemon {
            state,
            socket,
            socket_group,
        } => {
            Daemon::new(socket, state, socket_group)
                .await?
                .run()
                .await?;
        }
        NatMapCommand::Install { binary, group } => {
            crate::install::install_systemd(&binary, &group)?;
        }
    }
    Ok(())
}
