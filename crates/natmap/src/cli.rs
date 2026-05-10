use std::process::Command;

use clap::Parser;
use clap::Subcommand;
use color_eyre::Result;

use crate::docker_cli;

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
    // --- Static VM NATs ---
    /// Add or delete PREROUTING and FORWARD rules for DNAT
    Forward {
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
    /// Add or delete POSTROUTING rule for SNAT
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
    /// Add or delete Hairpin NAT rules
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
    /// Enable IP forwarding using sysctl
    EnableForwarding,
    /// Save iptables rules to /etc/iptables/rules.v4
    Persist,

    // --- Dynamic Docker NATs (formerly dockernatmap) ---
    /// List all active Docker port mappings
    List {
        /// Filter by container ID or name
        #[arg(value_name = "CONTAINER_ID")]
        container_id: Option<String>,
    },
    /// Remap a host port for a running Docker container
    Remap {
        /// Target container ID or name
        #[arg(value_name = "CONTAINER_ID")]
        container_id: String,
        /// Port mapping (e.g., 8080:9090)
        #[arg(value_name = "OLD_PORT:NEW_PORT")]
        mapping: String,
    },
    /// Add a new port mapping to a running Docker container
    Add {
        /// Target container ID or name
        #[arg(value_name = "CONTAINER_ID")]
        container_id: String,
        /// Port mapping (e.g., 8443:443/tcp or 10.0.0.1:8443:443/tcp)
        #[arg(value_name = "[HOST_IP:]HOST_PORT:CONTAINER_PORT[/PROTO]")]
        mapping: String,
    },
    /// Remove a specific Docker port mapping
    Remove {
        /// Target container ID or name
        #[arg(value_name = "CONTAINER_ID")]
        container_id: Option<String>,
        /// Port mapping to remove (e.g., 8080/tcp)
        #[arg(value_name = "PORT[/PROTO]")]
        port: Option<String>,
        /// Remove all port mappings for the container
        #[arg(long)]
        all: bool,
        /// Remove by mapping ID
        #[arg(long)]
        id: Option<u64>,
    },
    /// Run the natmap daemon (manages Docker mappings and cleans up stale static NATs)
    Daemon {
        /// State file path
        #[arg(long, default_value = "/var/lib/natmap/state.json")]
        state_dir: String,
        /// Unix socket path
        #[arg(long, default_value = "/run/natmap.sock")]
        socket: String,
        /// Group to own the Unix socket
        #[arg(long, default_value = "natmap")]
        socket_group: String,
    },
    /// Install natmap daemon as a systemd service
    Install {
        /// Group for socket access (user will be added to this group)
        #[arg(long, default_value = "natmap")]
        group: String,
        /// Path to the daemon binary
        #[arg(long, default_value = "/usr/local/bin/lab-ops")]
        binary: String,
    },
}

pub async fn run_cli_with_args(cli: Cli) -> Result<()> {
    let socket = cli.socket;
    let json = cli.json;

    match cli.command {
        // --- Static VM Commands ---
        NatMapCommand::Forward {
            ext_ip,
            int_ip,
            proto,
            ports,
            ext_if,
            delete,
        } => {
            handle_forward(ext_ip, int_ip, proto, ports, ext_if, delete)?;
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
        NatMapCommand::EnableForwarding => {
            let status = Command::new("sysctl")
                .arg("-w")
                .arg("net.ipv4.ip_forward=1")
                .status()?;
            if !status.success() {
                color_eyre::eyre::bail!("Failed to enable IP forwarding");
            }
        }
        NatMapCommand::Persist => {
            let status = Command::new("sh")
                .arg("-c")
                .arg("iptables-save > /etc/iptables/rules.v4")
                .status()?;
            if !status.success() {
                // Ignore error as in bash script (|| true)
            }
        }

        // --- Dynamic Docker Commands ---
        NatMapCommand::List { container_id } => {
            docker_cli::list(container_id, &socket, json).await?;
        }
        NatMapCommand::Remap {
            container_id,
            mapping,
        } => {
            docker_cli::remap(container_id, mapping, &socket, json).await?;
        }
        NatMapCommand::Add {
            container_id,
            mapping,
        } => {
            docker_cli::add(container_id, mapping, &socket, json).await?;
        }
        NatMapCommand::Remove {
            container_id,
            port,
            all,
            id,
        } => {
            docker_cli::remove(container_id, port, all, id, &socket, json).await?;
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

fn handle_forward(
    ext_ip: String,
    int_ip: String,
    proto: String,
    ports: String,
    ext_if: Option<String>,
    delete: bool,
) -> Result<()> {
    let action = if delete { "-D" } else { "-A" };
    let multiport = ports.contains(',');
    let port_args = if multiport {
        vec!["-m", "multiport", "--dports", &ports]
    } else {
        vec!["--dport", &ports]
    };

    let mut pre_args = vec!["-t", "nat", action, "PREROUTING"];
    if let Some(ref iface) = ext_if {
        pre_args.extend(vec!["-i", iface]);
    }
    pre_args.extend(vec!["-d", &ext_ip, "-p", &proto]);
    pre_args.extend(port_args.clone());

    let dest = if multiport {
        int_ip.clone()
    } else {
        format!("{}:{}", int_ip, ports)
    };
    pre_args.extend(vec!["-j", "DNAT", "--to-destination", &dest]);

    run_iptables(&pre_args, delete)?;

    let mut fwd_args = vec![action, "FORWARD", "-p", &proto, "-d", &int_ip];
    fwd_args.extend(port_args);
    fwd_args.extend(vec!["-j", "ACCEPT"]);

    run_iptables(&fwd_args, delete)?;
    Ok(())
}

fn handle_snat(int_ip: String, ext_if: String, ext_ip: String, delete: bool) -> Result<()> {
    let action = if delete { "-D" } else { "-A" };
    let args = vec![
        "-t",
        "nat",
        action,
        "POSTROUTING",
        "-s",
        &int_ip,
        "-o",
        &ext_if,
        "-j",
        "SNAT",
        "--to-source",
        &ext_ip,
    ];
    run_iptables(&args, delete)?;
    Ok(())
}

fn handle_hairpin(
    ext_ip: String,
    int_ip: String,
    proto: String,
    ports: String,
    delete: bool,
) -> Result<()> {
    let action = if delete { "-D" } else { "-A" };
    let multiport = ports.contains(',');
    let port_args = if multiport {
        vec!["-m", "multiport", "--dports", &ports]
    } else {
        vec!["--dport", &ports]
    };

    let mut pre_args = vec![
        "-t",
        "nat",
        action,
        "PREROUTING",
        "-s",
        &int_ip,
        "-d",
        &ext_ip,
        "-p",
        &proto,
    ];
    pre_args.extend(port_args.clone());
    pre_args.extend(vec!["-j", "DNAT", "--to-destination", &int_ip]);
    run_iptables(&pre_args, delete)?;

    let mut post_args = vec![
        "-t",
        "nat",
        action,
        "POSTROUTING",
        "-s",
        &int_ip,
        "-d",
        &int_ip,
        "-p",
        &proto,
    ];
    post_args.extend(port_args);
    post_args.extend(vec!["-j", "MASQUERADE"]);
    run_iptables(&post_args, delete)?;
    Ok(())
}

fn run_iptables(args: &[&str], ignore_error: bool) -> Result<()> {
    let output = match Command::new("iptables").args(args).output() {
        Ok(o) => o,
        Err(e) => {
            if ignore_error {
                return Ok(());
            } else {
                return Err(e.into());
            }
        }
    };
    if !output.status.success() && !ignore_error {
        let stderr = String::from_utf8_lossy(&output.stderr);
        color_eyre::eyre::bail!(
            "iptables command failed: iptables {}\n{}",
            args.join(" "),
            stderr
        );
    }
    Ok(())
}
