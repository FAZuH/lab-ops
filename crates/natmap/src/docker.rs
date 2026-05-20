//! Docker client helpers for discovering and inspecting container port mappings.

use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;

use bollard::Docker;
use color_eyre::Result;
use tracing::debug;

use crate::models::DockerPortMap;
use crate::models::DockerPortMapRequest;

/// Connects to the local Docker daemon via its default Unix socket.
pub fn connect() -> Result<Docker> {
    Ok(Docker::connect_with_socket_defaults()?)
}

/// Discovers all published port mappings for a container.
///
/// Inspects the container's network settings and parses its exposed ports
/// into [`DockerPortMap`] entries. Handles both IPv4 and IPv6 host
/// bindings when the host IP is unspecified (`0.0.0.0`).
pub async fn get_port_mappings(docker: &Docker, c_id: &str) -> Result<Vec<DockerPortMap>> {
    let inspect = docker.inspect_container(c_id, None).await?;
    let c_name = inspect
        .name
        .unwrap_or_else(|| "unknown".to_string())
        .trim_start_matches('/')
        .to_string();

    let Some(network_settings) = inspect.network_settings else {
        return Ok(vec![]);
    };

    // Find the primary container IP address. We check networks attached.
    let Some(c_ip) = network_settings.networks.as_ref().and_then(|networks| {
        networks.values().find_map(|net| {
            net.ip_address
                .as_ref()
                .filter(|ip| !ip.is_empty())
                .and_then(|ip| IpAddr::from_str(ip).ok())
        })
    }) else {
        debug!("Container {c_id} has no IP address, skipping ports");
        return Ok(vec![]);
    };

    let Some(ports) = network_settings.ports else {
        return Ok(vec![]);
    };

    let mut mappings = vec![];
    for (port_proto, bindings) in ports {
        let Some(bindings) = bindings else { continue };

        // Parse container port and proto, e.g., "80/tcp"
        let parts: Vec<&str> = port_proto.split('/').collect();
        if parts.len() != 2 {
            // this shouldn't happen
            continue;
        }

        let Ok(c_port) = u16::from_str(parts[0]) else {
            continue;
        };

        let Ok(proto) = parts[1].to_lowercase().try_into() else {
            continue;
        };

        let container_addr = SocketAddr::new(c_ip, c_port);

        for bind in bindings {
            let Some(host_port) = bind
                .host_port
                .as_deref()
                // Ignore ranges for now or parse the first port in range
                .and_then(|s| s.split('-').next())
                .and_then(|s| s.parse::<u16>().ok())
            else {
                continue;
            };

            let host_ip_str = bind.host_ip.as_deref().unwrap_or_default();
            let ips: &[&str] = if host_ip_str.is_empty() || host_ip_str == "0.0.0.0" {
                &["0.0.0.0", "::"]
            } else {
                &[host_ip_str]
            };
            mappings.extend(ips.iter().filter_map(|ip| {
                let host_ip = IpAddr::from_str(ip).ok()?;
                let req = DockerPortMapRequest {
                    host_addr: SocketAddr::new(host_ip, host_port),
                    container_addr,
                    proto,
                };
                Some(DockerPortMap::new(0, req, c_id.to_string(), c_name.clone()))
            }));
        }
    }

    Ok(mappings)
}
