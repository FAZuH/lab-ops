use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;

use bollard::Docker;
use color_eyre::Result;
use tracing::debug;

use crate::models::ActivePortMapping;
use crate::models::PortMappingRequest;
use crate::models::TransportProtocol;

pub fn connect() -> Result<Docker> {
    Ok(Docker::connect_with_socket_defaults()?)
}

pub async fn get_port_mappings(
    docker: &Docker,
    container_id: &str,
) -> Result<Vec<ActivePortMapping>> {
    let inspect = docker.inspect_container(container_id, None).await?;
    let container_name = inspect
        .name
        .unwrap_or_else(|| "unknown".to_string())
        .trim_start_matches('/')
        .to_string();

    let mut mappings = Vec::new();

    let network_settings = match inspect.network_settings {
        Some(ns) => ns,
        None => return Ok(mappings),
    };

    // Find the primary container IP address. We check networks attached.
    let mut container_ip = None;
    if let Some(networks) = &network_settings.networks {
        for net in networks.values() {
            if let Some(ip) = &net.ip_address
                && !ip.is_empty()
                && let Ok(addr) = IpAddr::from_str(ip)
            {
                container_ip = Some(addr);
                break;
            }
        }
    }

    let container_ip = match container_ip {
        Some(ip) => ip,
        None => {
            debug!(
                "Container {} has no IP address, skipping ports",
                container_id
            );
            return Ok(mappings);
        }
    };

    let ports = match network_settings.ports {
        Some(p) => p,
        None => return Ok(mappings),
    };

    for (port_proto, bindings) in ports {
        let bindings = match bindings {
            Some(b) => b,
            None => continue,
        };

        // Parse container port and proto, e.g., "80/tcp"
        let parts: Vec<&str> = port_proto.split('/').collect();
        if parts.len() != 2 {
            continue;
        }
        let c_port = match u16::from_str(parts[0]) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let proto = match parts[1].to_lowercase().as_str() {
            "tcp" => TransportProtocol::Tcp,
            "udp" => TransportProtocol::Udp,
            _ => continue,
        };

        let container_addr = SocketAddr::new(container_ip, c_port);

        for binding in bindings {
            let host_port_str = binding.host_port.unwrap_or_default();
            let host_ip_str = binding.host_ip.unwrap_or_default();

            // Ignore ranges for now or parse the first port in range
            let host_port =
                match u16::from_str(host_port_str.split('-').next().unwrap_or(&host_port_str)) {
                    Ok(p) => p,
                    Err(_) => continue,
                };

            let mut add_mapping = |ip_str: &str| {
                if let Ok(host_ip) = IpAddr::from_str(ip_str) {
                    let req = PortMappingRequest {
                        host_addr: SocketAddr::new(host_ip, host_port),
                        container_addr,
                        proto,
                    };
                    mappings.push(ActivePortMapping::new(
                        0,
                        req,
                        container_id.to_string(),
                        container_name.clone(),
                    ));
                }
            };

            if host_ip_str.is_empty() || host_ip_str == "0.0.0.0" {
                add_mapping("0.0.0.0");
                add_mapping("::"); // Add IPv6 as well mirroring Docker behavior
            } else {
                add_mapping(&host_ip_str);
            }
        }
    }

    Ok(mappings)
}
