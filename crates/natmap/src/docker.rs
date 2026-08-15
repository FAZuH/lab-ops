//! Docker client helpers for discovering and inspecting container port mappings.
//!
//! [`get_port_mappings`] is a thin wrapper over the shared lab-lib inspect
//! API: lab-lib owns the inspect parsing, and this module converts the shared
//! [`lab_ops_lab_lib::docker::PortMapping`] shape into natmap's [`DockerPortMap`] view.

use bollard::Docker;
use color_eyre::Result;

use crate::models::DockerPortMap;
use crate::models::DockerPortMapRequest;

/// Connects to the local Docker daemon via its default Unix socket.
pub fn connect() -> Result<Docker> {
    lab_ops_lab_lib::docker::connect()
}

/// Discovers all published port mappings for a container.
///
/// Inspects the container via the shared lab-lib inspect API and converts each
/// shared [`lab_ops_lab_lib::docker::PortMapping`] into a natmap [`DockerPortMap`].
pub async fn get_port_mappings(docker: &Docker, c_id: &str) -> Result<Vec<DockerPortMap>> {
    let inspect = docker.inspect_container(c_id, None).await?;
    let name = lab_ops_lab_lib::docker::parse_container_inspect(&inspect).name;

    Ok(lab_ops_lab_lib::docker::parse_port_mappings(&inspect)
        .into_iter()
        .map(|mapping| mapping_from_port_mapping(mapping, c_id, &name))
        .collect())
}

/// Converts a shared lab-lib [`lab_ops_lab_lib::docker::PortMapping`] into
/// natmap's [`DockerPortMap`].
///
/// An empty container name falls back to `"unknown"`. The container ID in the
/// map is the passed `c_id` — the rule comment format
/// (`natmap:<container_id>:<host_port>`) depends on it.
fn mapping_from_port_mapping(
    mapping: lab_ops_lab_lib::docker::PortMapping,
    c_id: &str,
    name: &str,
) -> DockerPortMap {
    let container_name = if name.is_empty() {
        "unknown".to_string()
    } else {
        name.to_string()
    };
    DockerPortMap::new(
        0,
        DockerPortMapRequest {
            host_addr: mapping.host_addr,
            container_addr: mapping.container_addr,
            proto: mapping.proto,
        },
        c_id.to_string(),
        container_name,
    )
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::net::SocketAddr;
    use std::str::FromStr;

    use lab_ops_lab_lib::docker::PortMapping;

    use super::*;
    use crate::models::TransportProtocol;

    // ── mapping_from_port_mapping ──

    fn make_mapping() -> PortMapping {
        PortMapping {
            host_addr: SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), 8080),
            container_addr: SocketAddr::new(IpAddr::from_str("172.17.0.2").unwrap(), 80),
            proto: TransportProtocol::Tcp,
        }
    }

    #[test]
    fn mapping_carries_fields_and_comment_uses_c_id() {
        let m = mapping_from_port_mapping(make_mapping(), "abc123", "my-nginx");

        assert_eq!(m.id, 0);
        assert_eq!(m.container_id, "abc123");
        assert_eq!(m.container_name, "my-nginx");
        assert_eq!(m.rule_comment, "natmap:abc123:8080");
        assert_eq!(
            m.request.host_addr,
            SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), 8080)
        );
        assert_eq!(
            m.request.container_addr,
            SocketAddr::new(IpAddr::from_str("172.17.0.2").unwrap(), 80)
        );
        assert_eq!(m.request.proto, TransportProtocol::Tcp);
    }

    #[test]
    fn mapping_empty_name_falls_back_to_unknown() {
        let m = mapping_from_port_mapping(make_mapping(), "c-id", "");

        assert_eq!(m.container_name, "unknown");
        assert_eq!(m.rule_comment, "natmap:c-id:8080");
    }

    #[test]
    fn mapping_ipv6_udp_preserved() {
        let m = mapping_from_port_mapping(
            PortMapping {
                host_addr: SocketAddr::new(IpAddr::from_str("::").unwrap(), 8443),
                container_addr: SocketAddr::new(IpAddr::from_str("::1").unwrap(), 443),
                proto: TransportProtocol::Udp,
            },
            "v6-id",
            "v6-svc",
        );

        assert!(m.request.is_ipv6());
        assert_eq!(m.request.proto, TransportProtocol::Udp);
        assert_eq!(m.rule_comment, "natmap:v6-id:8443");
        assert_eq!(m.container_name, "v6-svc");
    }
}
