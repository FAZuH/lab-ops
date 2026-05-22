//! Client for the natmap daemon over its Unix socket API.
//!
//! Communicates with the natmap daemon via [`natmap::cli::run_cli`] to manage
//! port mappings, DNAT rules, and hairpin NAT rules.

use std::net::IpAddr;
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::bail;
use lab_lib::TransportProtocol;
use natmap::cli::Cli;
use natmap::cli::DockerCommand;
use natmap::cli::NatMapCommand;

/// Client for the natmap daemon over its Unix socket.
#[derive(Debug)]
pub struct NatmapClient {
    socket: String,
}

impl NatmapClient {
    /// Create a client connected to the given natmap Unix socket path.
    #[allow(dead_code)]
    pub fn new(socket: String) -> Self {
        NatmapClient { socket }
    }

    /// Create a client using the `NATMAP_SOCKET` env var, defaulting to
    /// [`lab_lib::NATMAP_SOCKET`].
    pub fn default_socket() -> Self {
        let socket =
            std::env::var("NATMAP_SOCKET").unwrap_or_else(|_| lab_lib::NATMAP_SOCKET.into());
        NatmapClient { socket }
    }

    /// Install or delete a DNAT rule.
    pub async fn dnat(
        &self,
        ext_ip: &str,
        int_ip: &str,
        ports: &str,
        proto: &str,
        delete: bool,
    ) -> Result<()> {
        natmap::cli::run_cli(Cli {
            socket: self.socket.clone().into(),
            json: false,
            command: NatMapCommand::Dnat {
                ext_ip: ext_ip.to_string(),
                int_ip: int_ip.to_string(),
                proto: proto.to_string(),
                ports: ports.to_string(),
                ext_if: None,
                delete,
            },
        })
        .await
    }

    /// Install or delete a hairpin NAT rule.
    pub async fn hairpin(
        &self,
        ext_ip: &str,
        int_ip: &str,
        ports: &str,
        proto: &str,
        delete: bool,
    ) -> Result<()> {
        natmap::cli::run_cli(Cli {
            socket: self.socket.clone().into(),
            json: false,
            command: NatMapCommand::Hairpin {
                ext_ip: ext_ip.to_string(),
                int_ip: int_ip.to_string(),
                proto: proto.to_string(),
                ports: ports.to_string(),
                delete,
            },
        })
        .await
    }

    /// Add a Docker port mapping via `lab-ops natmap docker add`.
    ///
    /// If `target_ip` is set, the daemon skips Docker inspect and uses the
    /// given IP directly — used for local (non-Docker) services.
    ///
    /// Handles the 409 Conflict response (mapping already exists) as a
    /// non-fatal warning rather than an error.
    pub async fn add_docker_mapping(
        &self,
        container_id: &str,
        bind_ip: Option<&str>,
        host_port: u16,
        container_port: u16,
        protocol: TransportProtocol,
        target_ip: Option<&str>,
    ) -> Result<()> {
        let spec = match (bind_ip, target_ip) {
            (Some(ip), Some(tip)) => {
                format!("{ip}:{host_port}:{tip}:{container_port}/{protocol}")
            }
            (Some(ip), None) => format!("{ip}:{host_port}:{container_port}/{protocol}"),
            (None, Some(tip)) => format!("{host_port}:{tip}:{container_port}/{protocol}"),
            (None, None) => format!("{host_port}:{container_port}/{protocol}"),
        };
        let cli = Cli {
            socket: self.socket.clone().into(),
            json: false,
            command: NatMapCommand::Docker {
                cmd: DockerCommand::Add {
                    container_id: container_id.to_string(),
                    mapping: Some(spec),
                    name: None,
                },
            },
        };

        natmap::cli::run_cli(cli).await.or_else(|e| {
            let msg = e.to_string();
            if msg.contains("409") {
                tracing::warn!("natmap mapping already exists (409), continuing: {msg}");
                Ok(())
            } else if msg.contains("404") || msg.contains("Container not found") {
                tracing::warn!("Container not found (may have restarted), continuing: {msg}");
                Ok(())
            } else {
                Err(e).wrap_err("natmap command failed")
            }
        })
    }

    /// Remove a Docker port mapping by host port.
    #[allow(dead_code)]
    pub async fn remove_docker_mapping(&self, container_id: &str, host_port: u16) -> Result<()> {
        natmap::cli::run_cli(Cli {
            socket: self.socket.clone().into(),
            json: false,
            command: NatMapCommand::Docker {
                cmd: DockerCommand::Remove {
                    container_id: Some(container_id.to_string()),
                    port: Some(host_port.to_string()),
                    all: false,
                    id: None,
                    name: None,
                },
            },
        })
        .await
        .wrap_err("natmap command failed")
    }

    /// Query the Docker daemon (`docker inspect`) for a container's first
    /// network IP address. Used as a fallback when no `bind_ip` or
    /// `bind_interface` is configured.
    pub fn get_container_ip(&self, container_id: &str) -> Result<IpAddr> {
        let output = Command::new("docker")
            .args([
                "inspect",
                "-f",
                "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
                container_id,
            ])
            .output()
            .wrap_err("failed to run docker inspect")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("docker inspect failed: {}", stderr.trim());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let ip_str = stdout.trim();

        if ip_str.is_empty() {
            bail!("no IP address found for container");
        }

        ip_str
            .parse()
            .wrap_err_with(|| format!("invalid IP from docker inspect: {ip_str}"))
    }
}
