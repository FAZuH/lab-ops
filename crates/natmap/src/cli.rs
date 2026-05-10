use std::process::Command;

use clap::Parser;
use clap::Subcommand;
use color_eyre::Result;

use crate::command::*;

#[derive(Parser, Debug)]
#[command(
    name = "natmap",
    about = "Manage iptables NAT rules (static VMs & dynamic Docker)"
)]
pub struct Cli {
    #[arg(long, default_value = "/run/natmap.sock", global = true)]
    pub socket: String,

    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: NatMapCommand,
}

#[derive(Subcommand, Debug)]
pub enum NatMapCommand {
    /// Add or delete DNAT port forwarding rules
    #[command(name = "dnat")]
    Dnat {
        #[arg(long)]
        ext_ip: String,
        #[arg(long)]
        int_ip: String,
        #[arg(long, default_value = "tcp")]
        proto: String,
        #[arg(long)]
        ports: String,
        #[arg(long)]
        ext_if: Option<String>,
        #[arg(long)]
        delete: bool,
    },
    /// Add or delete SNAT rules
    #[command(name = "snat")]
    Snat {
        #[arg(long)]
        int_ip: String,
        #[arg(long)]
        ext_if: String,
        #[arg(long)]
        ext_ip: String,
        #[arg(long)]
        delete: bool,
    },
    /// Add or delete hairpin NAT rules
    #[command(name = "hairpin")]
    Hairpin {
        #[arg(long)]
        ext_ip: String,
        #[arg(long)]
        int_ip: String,
        #[arg(long, default_value = "tcp")]
        proto: String,
        #[arg(long)]
        ports: String,
        #[arg(long)]
        delete: bool,
    },
    /// List all NAT rules (static iptables + Docker mappings)
    #[command(name = "ls")]
    List {
        #[arg(value_name = "CONTAINER_ID")]
        container_id: Option<String>,
    },
    /// Docker container port mappings
    #[command(name = "docker")]
    Docker {
        #[command(subcommand)]
        cmd: DockerCommand,
    },
    /// Save iptables rules to /etc/iptables/rules.v4
    #[command(name = "save")]
    Save,
    /// Enable IP forwarding via sysctl
    #[command(name = "fwd")]
    Fwd,
    /// Run the natmap daemon
    #[command(name = "daemon")]
    Daemon {
        #[arg(long, default_value = "/var/lib/natmap/state.json")]
        state_dir: String,
        #[arg(long, default_value = "/run/natmap.sock")]
        socket: String,
        #[arg(long, default_value = "natmap")]
        socket_group: String,
    },
    /// Install natmap daemon as a systemd service
    #[command(name = "install")]
    Install {
        #[arg(long, default_value = "natmap")]
        group: String,
        #[arg(long, default_value = "/usr/local/bin/lab-ops")]
        binary: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum DockerCommand {
    /// Add a new port mapping to a running container
    #[command(name = "add")]
    Add {
        #[arg(value_name = "CONTAINER_ID")]
        container_id: String,
        #[arg(value_name = "[HOST_IP:]HOST_PORT:CONTAINER_PORT[/PROTO]")]
        mapping: String,
    },
    /// Remove a specific Docker port mapping
    #[command(name = "rm")]
    Remove {
        #[arg(value_name = "CONTAINER_ID")]
        container_id: Option<String>,
        #[arg(value_name = "PORT[/PROTO]")]
        port: Option<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        id: Option<u64>,
    },
    /// Remap a host port for a running container
    #[command(name = "remap")]
    Remap {
        #[arg(value_name = "CONTAINER_ID")]
        container_id: String,
        #[arg(value_name = "OLD_PORT:NEW_PORT")]
        mapping: String,
    },
}

pub async fn run_cli_with_args(cli: Cli) -> Result<()> {
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
        } => {
            handle_dnat(ext_ip, int_ip, proto, ports, ext_if, delete)?;
        }
        NatMapCommand::Snat {
            int_ip,
            ext_if,
            ext_ip,
            delete,
        } => {
            handle_snat(int_ip, ext_if, ext_ip, delete)?;
        }
        NatMapCommand::Hairpin {
            ext_ip,
            int_ip,
            proto,
            ports,
            delete,
        } => {
            handle_hairpin(ext_ip, int_ip, proto, ports, delete)?;
        }
        NatMapCommand::List { container_id } => {
            handle_list(&socket, container_id, json).await?;
        }
        NatMapCommand::Docker { cmd } => match cmd {
            DockerCommand::Add {
                container_id,
                mapping,
            } => {
                add(container_id, mapping, &socket, json).await?;
            }
            DockerCommand::Remove {
                container_id,
                port,
                all,
                id,
            } => {
                remove(container_id, port, all, id, &socket, json).await?;
            }
            DockerCommand::Remap {
                container_id,
                mapping,
            } => {
                remap(container_id, mapping, &socket, json).await?;
            }
        },
        NatMapCommand::Save => {
            let status = Command::new("sh")
                .arg("-c")
                .arg("iptables-save > /etc/iptables/rules.v4")
                .status()?;
            if !status.success() {
                // Ignore error as in bash script (|| true)
            }
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
            state_dir,
            socket: daemon_socket,
            socket_group,
        } => {
            crate::daemon::run_daemon_with_paths(&daemon_socket, &state_dir, &socket_group).await?;
        }
        NatMapCommand::Install { binary, group } => {
            crate::install::install_systemd(&binary, &group)?;
        }
    }
    Ok(())
}
