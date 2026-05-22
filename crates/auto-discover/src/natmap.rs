//! CLI client for the natmap daemon.
//!
//! Communicates with the natmap daemon via `lab-ops natmap` CLI invocations
//! to manage Docker port mappings and query container IPs.

use std::net::IpAddr;
use std::process::Command;

use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::bail;

/// Client that invokes `lab-ops natmap` to add/remove Docker port mappings.
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

    /// Add a Docker port mapping via `lab-ops natmap docker add`.
    ///
    /// If `target_ip` is set, the daemon skips Docker inspect and uses the
    /// given IP directly — used for local (non-Docker) services.
    ///
    /// Handles the 409 Conflict response (mapping already exists) as a
    /// non-fatal warning rather than an error.
    pub fn add_docker_mapping(
        &self,
        container_id: &str,
        bind_ip: Option<&str>,
        host_port: u16,
        container_port: u16,
        protocol: &str,
        target_ip: Option<&str>,
    ) -> Result<()> {
        let spec = match (bind_ip, target_ip) {
            (Some(ip), Some(tip)) => {
                format!("{ip}:{host_port}:{tip}:{container_port}/{protocol}")
            }
            (Some(ip), None) => {
                format!("{ip}:{host_port}:{container_port}/{protocol}")
            }
            (None, Some(tip)) => {
                format!("{host_port}:{tip}:{container_port}/{protocol}")
            }
            (None, None) => format!("{host_port}:{container_port}/{protocol}"),
        };
        self.run_docker_cmd("add", container_id, &spec)
    }

    /// Remove a Docker port mapping by host port.
    #[allow(dead_code)]
    pub fn remove_docker_mapping(&self, container_id: &str, host_port: u16) -> Result<()> {
        self.run_docker_cmd("rm", container_id, &host_port.to_string())
    }

    fn run_docker_cmd(&self, cmd: &str, container_id: &str, arg: &str) -> Result<()> {
        let output = Command::new("lab-ops")
            .args([
                "natmap",
                "--socket",
                &self.socket,
                "docker",
                cmd,
                container_id,
                arg,
            ])
            .output()
            .wrap_err("failed to run lab-ops natmap docker")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if cmd == "add" && stderr.contains("409") {
                tracing::warn!(
                    "natmap mapping already exists (409), continuing: {}",
                    stderr.trim()
                );
                return Ok(());
            }
            bail!("lab-ops natmap docker failed: {}", stderr.trim());
        }
        Ok(())
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
