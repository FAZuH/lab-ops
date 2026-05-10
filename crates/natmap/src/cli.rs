use std::process::Command;

use clap::Parser;
use clap::Subcommand;
use color_eyre::Result;

#[derive(Parser, Debug)]
#[command(name = "natmap", about = "Manage iptables NAT rules")]
pub struct Cli {
    #[command(subcommand)]
    pub command: NatMapCommand,
}

#[derive(Subcommand, Debug)]
pub enum NatMapCommand {
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
}

pub fn run_cli_with_args(cli: Cli) -> Result<()> {
    match cli.command {
        NatMapCommand::Forward {
            ext_ip,
            int_ip,
            proto,
            ports,
            ext_if,
            delete,
        } => {
            let action = if delete { "-D" } else { "-A" };
            let multiport = ports.contains(',');
            let port_args = if multiport {
                vec!["-m", "multiport", "--dports", &ports]
            } else {
                vec!["--dport", &ports]
            };

            // PREROUTING
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

            // FORWARD
            let mut fwd_args = vec![action, "FORWARD", "-p", &proto, "-d", &int_ip];
            fwd_args.extend(port_args);
            fwd_args.extend(vec!["-j", "ACCEPT"]);

            run_iptables(&fwd_args, delete)?;
        }
        NatMapCommand::Snat {
            int_ip,
            ext_if,
            ext_ip,
            delete,
        } => {
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
        }
        NatMapCommand::Hairpin {
            ext_ip,
            int_ip,
            proto,
            ports,
            delete,
        } => {
            let action = if delete { "-D" } else { "-A" };
            let multiport = ports.contains(',');
            let port_args = if multiport {
                vec!["-m", "multiport", "--dports", &ports]
            } else {
                vec!["--dport", &ports]
            };

            // PREROUTING
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

            // POSTROUTING
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
    }
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
