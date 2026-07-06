//! Docker client helpers for discovering and inspecting container port mappings.

use std::net::IpAddr;
use std::net::SocketAddr;
use std::str::FromStr;

use bollard::Docker;
use color_eyre::Result;

use crate::models::DockerPortMap;
use crate::models::DockerPortMapRequest;
use crate::models::TransportProtocol;

/// Connects to the local Docker daemon via its default Unix socket.
pub fn connect() -> Result<Docker> {
    lab_ops_lab_lib::docker::connect()
}

/// Resolves a Docker host IP string into the list of IP addresses to bind.
///
/// When the host IP is empty or `"0.0.0.0"`, returns both `"0.0.0.0"` and
/// `"::"` so that the mapping works for both IPv4 and IPv6 traffic.
fn resolve_host_ips(host_ip: &str) -> Vec<&str> {
    if host_ip.is_empty() || host_ip == "0.0.0.0" {
        vec!["0.0.0.0", "::"]
    } else {
        vec![host_ip]
    }
}

/// Parses a Docker container port/protocol string (e.g. `"80/tcp"`).
fn parse_container_port_proto(s: &str) -> Option<(u16, TransportProtocol)> {
    let mut parts = s.splitn(2, '/');
    let port_str = parts.next()?;
    let proto_str = parts.next()?;
    let port = port_str.parse().ok()?;
    let proto = proto_str.to_lowercase().parse().ok()?;
    Some((port, proto))
}

/// Parses a host port string from a Docker `PortBinding`, handling ranges.
///
/// When the string contains a range (e.g. `"3000-3005"`), returns the first
/// port in the range.
fn parse_host_port(s: &str) -> Option<u16> {
    s.split('-').next().and_then(|p| p.parse().ok())
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
        .as_deref()
        .map(lab_ops_lab_lib::docker::trim_container_name)
        .unwrap_or("unknown")
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
        tracing::debug!(container.id = %c_id, "container has no IP address, skipping ports");
        return Ok(vec![]);
    };

    let Some(ports) = network_settings.ports else {
        return Ok(vec![]);
    };

    let mut mappings = vec![];
    for (port_proto, bindings) in ports {
        let Some(bindings) = bindings else { continue };

        let Some((c_port, proto)) = parse_container_port_proto(&port_proto) else {
            continue;
        };

        let container_addr = SocketAddr::new(c_ip, c_port);

        for bind in bindings {
            let Some(host_port) = bind
                .host_port
                .as_deref()
                .and_then(parse_host_port)
            else {
                continue;
            };

            let host_ip_str = bind.host_ip.as_deref().unwrap_or_default();
            let ips = resolve_host_ips(host_ip_str);
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

#[cfg(test)]
mod tests {
    use super::*;

    // ── resolve_host_ips ──

    #[test]
    fn resolve_ips_empty_returns_both() {
        assert_eq!(resolve_host_ips(""), vec!["0.0.0.0", "::"]);
    }

    #[test]
    fn resolve_ips_unspecified_ipv4_returns_both() {
        assert_eq!(resolve_host_ips("0.0.0.0"), vec!["0.0.0.0", "::"]);
    }

    #[test]
    fn resolve_ips_specific_ipv4_returns_single() {
        assert_eq!(resolve_host_ips("192.168.1.100"), vec!["192.168.1.100"]);
    }

    #[test]
    fn resolve_ips_ipv6_returns_single() {
        assert_eq!(resolve_host_ips("::1"), vec!["::1"]);
    }

    #[test]
    fn resolve_ips_loopback_returns_single() {
        assert_eq!(resolve_host_ips("127.0.0.1"), vec!["127.0.0.1"]);
    }

    // ── parse_container_port_proto ──

    #[test]
    fn parse_port_proto_tcp() {
        let (port, proto) = parse_container_port_proto("80/tcp").unwrap();
        assert_eq!(port, 80);
        assert_eq!(proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_port_proto_udp() {
        let (port, proto) = parse_container_port_proto("19132/udp").unwrap();
        assert_eq!(port, 19132);
        assert_eq!(proto, TransportProtocol::Udp);
    }

    #[test]
    fn parse_port_proto_no_proto_returns_none() {
        assert!(parse_container_port_proto("80").is_none());
    }

    #[test]
    fn parse_port_proto_invalid_port_returns_none() {
        assert!(parse_container_port_proto("abc/tcp").is_none());
    }

    #[test]
    fn parse_port_proto_invalid_proto_returns_none() {
        assert!(parse_container_port_proto("80/xyz").is_none());
    }

    #[test]
    fn parse_port_proto_empty_string_returns_none() {
        assert!(parse_container_port_proto("").is_none());
    }

    #[test]
    fn parse_port_proto_uppercase_proto() {
        let (port, proto) = parse_container_port_proto("443/TCP").unwrap();
        assert_eq!(port, 443);
        assert_eq!(proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_port_proto_port_zero() {
        let (port, proto) = parse_container_port_proto("0/udp").unwrap();
        assert_eq!(port, 0);
        assert_eq!(proto, TransportProtocol::Udp);
    }

    #[test]
    fn parse_port_proto_max_port() {
        let (port, _proto) = parse_container_port_proto("65535/tcp").unwrap();
        assert_eq!(port, 65535);
    }

    // ── parse_host_port ──

    #[test]
    fn parse_host_port_simple() {
        assert_eq!(parse_host_port("8080"), Some(8080));
    }

    #[test]
    fn parse_host_port_range_returns_first() {
        assert_eq!(parse_host_port("3000-3005"), Some(3000));
    }

    #[test]
    fn parse_host_port_empty_returns_none() {
        assert!(parse_host_port("").is_none());
    }

    #[test]
    fn parse_host_port_invalid_returns_none() {
        assert!(parse_host_port("abc").is_none());
    }

    #[test]
    fn parse_host_port_zero() {
        assert_eq!(parse_host_port("0"), Some(0));
    }

    #[test]
    fn parse_host_port_range_from_zero() {
        assert_eq!(parse_host_port("0-1023"), Some(0));
    }
}
