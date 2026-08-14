//! Docker API client for listing running containers.

use std::net::IpAddr;
use std::process::Command as ProcessCommand;

use bollard::Docker;
use bollard::plugin::ContainerSummary;
use bollard::query_parameters::ListContainersOptionsBuilder;
use color_eyre::Result;
use color_eyre::eyre::WrapErr;
use color_eyre::eyre::bail;

use crate::model::ContainerInfo;

/// Parses `docker inspect` output to extract a container's first network IP.
fn parse_docker_inspect_output(output: &str) -> Result<IpAddr> {
    let ip_str = output.trim();
    if ip_str.is_empty() {
        bail!("no IP address found for container");
    }
    ip_str
        .parse()
        .wrap_err_with(|| format!("invalid IP from docker inspect: {ip_str}"))
}

/// Runs `docker inspect` for a container's first network IP address.
///
/// Used as a fallback when no `bind_ip` or `bind_interface` is configured.
pub fn get_container_ip(container_id: &str) -> Result<IpAddr> {
    let output = ProcessCommand::new("docker")
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
    parse_docker_inspect_output(&stdout)
}

/// Wrapper around the Bollard Docker API client.
pub struct DockerClient {
    docker: Docker,
}

impl From<ContainerSummary> for ContainerInfo {
    fn from(c: ContainerSummary) -> Self {
        let id = c.id.unwrap_or_default();
        let name = c
            .names
            .unwrap_or_default()
            .first()
            .cloned()
            .map(|n| lab_ops_lab_lib::docker::trim_container_name(&n).to_string())
            .unwrap_or_default();
        let compose_project = c
            .labels
            .as_ref()
            .and_then(|labels| labels.get("com.docker.compose.project"))
            .cloned();

        Self {
            id,
            name,
            compose_project,
        }
    }
}

impl DockerClient {
    /// Connect to the local Docker daemon via the default socket or
    /// `DOCKER_HOST` env var.
    pub fn new() -> Result<Self> {
        let docker = lab_ops_lab_lib::docker::connect()?;
        Ok(DockerClient { docker })
    }

    /// Inspect all running containers.
    pub async fn list_running_containers(&self) -> Result<Vec<ContainerInfo>> {
        let options = ListContainersOptionsBuilder::default().all(false).build();

        let infos = self
            .docker
            .list_containers(Some(options))
            .await
            .wrap_err("failed to list Docker containers")?
            .into_iter()
            .map(|c| c.into())
            .collect();

        Ok(infos)
    }

    /// Inspect a single container.
    pub async fn inspect_container(&self, container_id: impl AsRef<str>) -> Result<ContainerInfo> {
        let id = container_id.as_ref();
        let info = self
            .docker
            .inspect_container(id, None)
            .await
            .wrap_err_with(|| format!("failed to inspect container {id}"))?;
        let name = info
            .name
            .as_deref()
            .map(lab_ops_lab_lib::docker::trim_container_name)
            .unwrap_or("")
            .to_string();
        let compose_project = info
            .config
            .as_ref()
            .and_then(|c| c.labels.as_ref())
            .and_then(|labels| labels.get("com.docker.compose.project"))
            .cloned();

        Ok(ContainerInfo {
            id: id.to_string(),
            name,
            compose_project,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    // ── parse_docker_inspect_output ──

    #[test]
    fn parse_docker_inspect_output_valid_ipv4() {
        let ip = parse_docker_inspect_output("172.17.0.2\n").unwrap();
        assert_eq!(ip, IpAddr::from_str("172.17.0.2").unwrap());
    }

    #[test]
    fn parse_docker_inspect_output_valid_ipv6() {
        let ip = parse_docker_inspect_output("2001:db8::1\n").unwrap();
        assert_eq!(ip, IpAddr::from_str("2001:db8::1").unwrap());
    }

    #[test]
    fn parse_docker_inspect_output_trimmed() {
        let ip = parse_docker_inspect_output("  10.0.0.5  \n").unwrap();
        assert_eq!(ip, IpAddr::from_str("10.0.0.5").unwrap());
    }

    #[test]
    fn parse_docker_inspect_output_empty_errors() {
        assert!(parse_docker_inspect_output("").is_err());
    }

    #[test]
    fn parse_docker_inspect_output_whitespace_only_errors() {
        assert!(parse_docker_inspect_output("  \n  ").is_err());
    }

    #[test]
    fn parse_docker_inspect_output_invalid_ip_errors() {
        assert!(parse_docker_inspect_output("not-an-ip").is_err());
    }
}
