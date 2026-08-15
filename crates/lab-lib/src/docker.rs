//! Docker client helpers and container inspection.
//!
//! Provides the [`connect`] and [`trim_container_name`] helpers, the
//! [`DockerClient`] wrapper around the Bollard API, and the pure inspect
//! parsers [`parse_container_inspect`] and [`parse_port_mappings`] that turn a
//! docker inspect response into the shared [`ContainerInfo`] and
//! [`PortMapping`] shapes used by both natmap and auto-discover.

use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;

use bollard::Docker;
use bollard::models::ContainerInspectResponse;
use bollard::models::ContainerSummary;
use bollard::models::EndpointSettings;
use bollard::query_parameters::ListContainersOptionsBuilder;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;

use crate::protocol::TransportProtocol;

/// Connect to the local Docker daemon via the default socket or `DOCKER_HOST` env var.
pub fn connect() -> Result<Docker> {
    Ok(Docker::connect_with_local_defaults()?)
}

/// Strip the leading `/` that Docker prepends to container names.
///
/// Docker names come in the format `"/example-drive"`. This returns `"example-drive"`.
pub fn trim_container_name(name: &str) -> &str {
    name.trim_start_matches('/')
}

// --- Inspection types ---

/// Settings for one network a container is attached to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerNetwork {
    /// Network name.
    pub name: String,
    /// IPv4 address of the container on this network.
    pub ip: Option<IpAddr>,
    /// Gateway address of the container on this network.
    pub gateway: Option<IpAddr>,
}

/// Metadata about a running Docker container.
///
/// The bare `id`, `name`, and `compose_project` fields come from the container
/// summary; `ip` and `networks` are filled in from network settings when the
/// inspect (or summary) response carries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    /// Full container ID.
    pub id: String,
    /// Container name (leading `/` stripped).
    pub name: String,
    /// Docker Compose project name from the `com.docker.compose.project` label.
    pub compose_project: Option<String>,
    /// Primary container IP: the first attached network (sorted by name) with
    /// a non-empty address.
    pub ip: Option<IpAddr>,
    /// Per-network endpoint settings, sorted by network name.
    pub networks: Vec<ContainerNetwork>,
}

/// A published port mapping from a host address to a container address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMapping {
    /// Address the host binds to.
    pub host_addr: SocketAddr,
    /// Address of the container port behind the mapping.
    pub container_addr: SocketAddr,
    /// Transport protocol.
    pub proto: TransportProtocol,
}

// --- Inspect parsing ---

/// Parses a docker inspect response into the shared container shape.
pub fn parse_container_inspect(inspect: &ContainerInspectResponse) -> ContainerInfo {
    let networks = parse_networks(
        inspect
            .network_settings
            .as_ref()
            .and_then(|ns| ns.networks.as_ref()),
    );
    let ip = primary_ip(&networks);
    let name = inspect
        .name
        .as_deref()
        .map(trim_container_name)
        .unwrap_or_default()
        .to_string();
    let compose_project = inspect
        .config
        .as_ref()
        .and_then(|c| c.labels.as_ref())
        .and_then(|labels| labels.get("com.docker.compose.project"))
        .cloned();

    ContainerInfo {
        id: inspect.id.clone().unwrap_or_default(),
        name,
        compose_project,
        ip,
        networks,
    }
}

/// Parses the published port mappings from a docker inspect response.
///
/// A container without an IP address yields no mappings. An unspecified host
/// IP (`""` or `0.0.0.0`) produces both an IPv4 and an IPv6 mapping.
pub fn parse_port_mappings(inspect: &ContainerInspectResponse) -> Vec<PortMapping> {
    let Some(network_settings) = inspect.network_settings.as_ref() else {
        return vec![];
    };

    let networks = parse_networks(network_settings.networks.as_ref());
    let Some(container_ip) = primary_ip(&networks) else {
        tracing::debug!(container.id = %inspect.id.as_deref().unwrap_or_default(), "container has no IP address, skipping ports");
        return vec![];
    };

    let Some(ports) = network_settings.ports.as_ref() else {
        return vec![];
    };

    let mut mappings = Vec::new();
    for (port_proto, bindings) in ports {
        let Some(bindings) = bindings else { continue };
        let Some((container_port, proto)) = parse_container_port_proto(port_proto) else {
            continue;
        };
        let container_addr = SocketAddr::new(container_ip, container_port);

        for bind in bindings {
            let Some(host_port) = bind.host_port.as_deref().and_then(parse_host_port) else {
                continue;
            };
            let host_ip = bind.host_ip.as_deref().unwrap_or_default();
            for host_ip in resolve_host_ips(host_ip) {
                let Ok(host_ip) = host_ip.parse() else {
                    continue;
                };
                mappings.push(PortMapping {
                    host_addr: SocketAddr::new(host_ip, host_port),
                    container_addr,
                    proto,
                });
            }
        }
    }

    mappings
}

/// Converts per-network endpoint settings into the shared network shape,
/// sorted by network name for a deterministic primary-IP selection.
fn parse_networks(networks: Option<&HashMap<String, EndpointSettings>>) -> Vec<ContainerNetwork> {
    let mut parsed: Vec<ContainerNetwork> = networks
        .into_iter()
        .flat_map(|map| map.iter())
        .map(|(name, endpoint)| ContainerNetwork {
            name: name.clone(),
            ip: parse_ip(endpoint.ip_address.as_deref()),
            gateway: parse_ip(endpoint.gateway.as_deref()),
        })
        .collect();
    parsed.sort_by(|a, b| a.name.cmp(&b.name));
    parsed
}

/// Parses an IP address string, treating empty strings as absent.
fn parse_ip(s: Option<&str>) -> Option<IpAddr> {
    s.filter(|s| !s.is_empty()).and_then(|s| s.parse().ok())
}

/// Returns the first network IP as the container's primary IP.
fn primary_ip(networks: &[ContainerNetwork]) -> Option<IpAddr> {
    networks.iter().find_map(|n| n.ip)
}

/// Parses a container port/protocol key (e.g. `"80/tcp"`).
fn parse_container_port_proto(s: &str) -> Option<(u16, TransportProtocol)> {
    let (port_str, proto_str) = s.split_once('/')?;

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

// --- Docker client ---

/// Wrapper around the Bollard Docker API client.
pub struct DockerClient {
    docker: Docker,
}

impl DockerClient {
    /// Connect to the local Docker daemon via the default socket or
    /// `DOCKER_HOST` env var.
    pub fn new() -> Result<Self> {
        let docker = connect()?;
        Ok(DockerClient { docker })
    }

    /// List all running containers.
    pub async fn list_running_containers(&self) -> Result<Vec<ContainerInfo>> {
        let options = ListContainersOptionsBuilder::default().all(false).build();

        let infos = self
            .docker
            .list_containers(Some(options))
            .await
            .wrap_err("failed to list Docker containers")?
            .into_iter()
            .map(ContainerInfo::from)
            .collect();

        Ok(infos)
    }

    /// Inspect a single container.
    pub async fn inspect_container(&self, container_id: impl AsRef<str>) -> Result<ContainerInfo> {
        let id = container_id.as_ref();
        let inspect = self
            .docker
            .inspect_container(id, None)
            .await
            .wrap_err_with(|| format!("failed to inspect container {id}"))?;
        Ok(parse_container_inspect(&inspect))
    }
}

impl From<ContainerSummary> for ContainerInfo {
    fn from(c: ContainerSummary) -> Self {
        let networks = parse_networks(
            c.network_settings
                .as_ref()
                .and_then(|ns| ns.networks.as_ref()),
        );
        let ip = primary_ip(&networks);
        let id = c.id.unwrap_or_default();
        let name = c
            .names
            .unwrap_or_default()
            .first()
            .cloned()
            .map(|n| trim_container_name(&n).to_string())
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
            ip,
            networks,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::str::FromStr;

    use bollard::models::ContainerInspectResponse;
    use serde_json::from_str;

    use super::*;

    // ── Fixtures ──

    const INSPECT_SINGLE_NETWORK: &str = r#"{
        "Id": "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90",
        "Name": "/web",
        "Config": {
            "Labels": {
                "com.docker.compose.project": "frontend"
            }
        },
        "NetworkSettings": {
            "Ports": {
                "80/tcp": [
                    { "HostIp": "0.0.0.0", "HostPort": "8080" }
                ]
            },
            "Networks": {
                "bridge": {
                    "IPAddress": "172.17.0.2",
                    "Gateway": "172.17.0.1"
                }
            }
        }
    }"#;

    const INSPECT_MULTI_NETWORK: &str = r#"{
        "Id": "multi-net-id",
        "Name": "/app",
        "Config": {
            "Labels": {
                "com.docker.compose.project": "stack"
            }
        },
        "NetworkSettings": {
            "Ports": {
                "443/tcp": [
                    { "HostIp": "127.0.0.1", "HostPort": "8443" }
                ]
            },
            "Networks": {
                "frontend": {
                    "IPAddress": "10.0.1.5",
                    "Gateway": "10.0.1.1"
                },
                "backend": {
                    "IPAddress": "10.0.2.5",
                    "Gateway": "10.0.2.1"
                }
            }
        }
    }"#;

    const INSPECT_EMPTY_FIELDS: &str = r#"{
        "Id": "empty-fields-id",
        "NetworkSettings": {
            "Networks": {
                "none": {
                    "IPAddress": "",
                    "Gateway": ""
                }
            }
        }
    }"#;

    const INSPECT_NO_NETWORKS: &str = r#"{
        "Id": "no-net-id",
        "Name": "/isolated",
        "NetworkSettings": {}
    }"#;

    const INSPECT_NO_NETWORK_SETTINGS: &str = r#"{
        "Id": "no-settings-id",
        "Name": "/bare"
    }"#;

    /// Parses a canned inspect JSON document into the bollard response type.
    fn make_inspect(json: &str) -> ContainerInspectResponse {
        from_str(json).expect("canned inspect fixture must deserialize")
    }

    // ── parse_container_inspect ──

    #[test]
    fn parse_container_inspect_single_network() {
        let info = parse_container_inspect(&make_inspect(INSPECT_SINGLE_NETWORK));

        assert_eq!(
            info,
            ContainerInfo {
                id: "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90".to_string(),
                name: "web".to_string(),
                compose_project: Some("frontend".to_string()),
                ip: Some(IpAddr::from_str("172.17.0.2").unwrap()),
                networks: vec![ContainerNetwork {
                    name: "bridge".to_string(),
                    ip: Some(IpAddr::from_str("172.17.0.2").unwrap()),
                    gateway: Some(IpAddr::from_str("172.17.0.1").unwrap()),
                }],
            }
        );
    }

    #[test]
    fn parse_container_inspect_multiple_networks() {
        let info = parse_container_inspect(&make_inspect(INSPECT_MULTI_NETWORK));

        assert_eq!(info.name, "app");
        assert_eq!(info.compose_project.as_deref(), Some("stack"));
        // Networks sorted by name; primary IP is the first sorted network.
        assert_eq!(
            info.networks,
            vec![
                ContainerNetwork {
                    name: "backend".to_string(),
                    ip: Some(IpAddr::from_str("10.0.2.5").unwrap()),
                    gateway: Some(IpAddr::from_str("10.0.2.1").unwrap()),
                },
                ContainerNetwork {
                    name: "frontend".to_string(),
                    ip: Some(IpAddr::from_str("10.0.1.5").unwrap()),
                    gateway: Some(IpAddr::from_str("10.0.1.1").unwrap()),
                },
            ]
        );
        assert_eq!(info.ip, Some(IpAddr::from_str("10.0.2.5").unwrap()));
    }

    #[test]
    fn parse_container_inspect_empty_fields() {
        let info = parse_container_inspect(&make_inspect(INSPECT_EMPTY_FIELDS));

        assert_eq!(info.id, "empty-fields-id");
        assert_eq!(info.name, "");
        assert_eq!(info.compose_project, None);
        assert_eq!(info.ip, None);
        assert_eq!(
            info.networks,
            vec![ContainerNetwork {
                name: "none".to_string(),
                ip: None,
                gateway: None,
            }]
        );
    }

    #[test]
    fn parse_container_inspect_no_networks() {
        let info = parse_container_inspect(&make_inspect(INSPECT_NO_NETWORKS));

        assert_eq!(info.name, "isolated");
        assert_eq!(info.ip, None);
        assert!(info.networks.is_empty());
    }

    #[test]
    fn parse_container_inspect_no_network_settings() {
        let info = parse_container_inspect(&make_inspect(INSPECT_NO_NETWORK_SETTINGS));

        assert_eq!(info.name, "bare");
        assert_eq!(info.ip, None);
        assert!(info.networks.is_empty());
    }

    // ── From<ContainerSummary> ──

    #[test]
    fn container_info_from_summary_with_networks() {
        let summary: bollard::models::ContainerSummary = from_str(
            r#"{
                "Id": "sum-id",
                "Names": ["/db"],
                "Labels": {
                    "com.docker.compose.project": "data"
                },
                "NetworkSettings": {
                    "Networks": {
                        "bridge": {
                            "IPAddress": "172.17.0.9",
                            "Gateway": "172.17.0.1"
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let info = ContainerInfo::from(summary);

        assert_eq!(info.id, "sum-id");
        assert_eq!(info.name, "db");
        assert_eq!(info.compose_project.as_deref(), Some("data"));
        assert_eq!(info.ip, Some(IpAddr::from_str("172.17.0.9").unwrap()));
        assert_eq!(info.networks.len(), 1);
        assert_eq!(info.networks[0].name, "bridge");
    }

    #[test]
    fn container_info_from_summary_without_networks() {
        let summary: bollard::models::ContainerSummary =
            from_str(r#"{ "Id": "bare-id", "Names": ["/bare"] }"#).unwrap();

        let info = ContainerInfo::from(summary);

        assert_eq!(info.name, "bare");
        assert_eq!(info.compose_project, None);
        assert_eq!(info.ip, None);
        assert!(info.networks.is_empty());
    }

    // ── parse_port_mappings ──

    #[test]
    fn parse_port_mappings_unspecified_host_ip_returns_v4_and_v6() {
        let inspect = make_inspect(INSPECT_SINGLE_NETWORK);

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 2);
        assert_eq!(
            mappings[0].host_addr,
            SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), 8080)
        );
        assert_eq!(
            mappings[0].container_addr,
            SocketAddr::new(IpAddr::from_str("172.17.0.2").unwrap(), 80)
        );
        assert_eq!(mappings[0].proto, TransportProtocol::Tcp);
        assert_eq!(
            mappings[1].host_addr,
            SocketAddr::new(IpAddr::from_str("::").unwrap(), 8080)
        );
        assert_eq!(
            mappings[1].container_addr,
            SocketAddr::new(IpAddr::from_str("172.17.0.2").unwrap(), 80)
        );
    }

    #[test]
    fn parse_port_mappings_specific_host_ip_returns_single() {
        let inspect = make_inspect(INSPECT_MULTI_NETWORK);

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 1);
        assert_eq!(
            mappings[0].host_addr,
            SocketAddr::new(IpAddr::from_str("127.0.0.1").unwrap(), 8443)
        );
        // Primary IP comes from the first sorted network (backend).
        assert_eq!(
            mappings[0].container_addr,
            SocketAddr::new(IpAddr::from_str("10.0.2.5").unwrap(), 443)
        );
        assert_eq!(mappings[0].proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_port_mappings_udp_port() {
        let inspect = make_inspect(
            r#"{
                "Id": "udp-id",
                "NetworkSettings": {
                    "Ports": {
                        "19132/udp": [
                            { "HostIp": "0.0.0.0", "HostPort": "19132" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.3" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 2);
        assert_eq!(mappings[0].proto, TransportProtocol::Udp);
        assert_eq!(mappings[0].host_addr.port(), 19132);
        assert_eq!(mappings[0].container_addr.port(), 19132);
    }

    #[test]
    fn parse_port_mappings_host_port_range_uses_first() {
        let inspect = make_inspect(
            r#"{
                "Id": "range-id",
                "NetworkSettings": {
                    "Ports": {
                        "3000/tcp": [
                            { "HostIp": "0.0.0.0", "HostPort": "3000-3005" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.4" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 2);
        assert_eq!(mappings[0].host_addr.port(), 3000);
    }

    #[test]
    fn parse_port_mappings_null_bindings_skipped() {
        let inspect = make_inspect(
            r#"{
                "Id": "null-id",
                "NetworkSettings": {
                    "Ports": {
                        "80/tcp": null,
                        "443/tcp": [
                            { "HostIp": "127.0.0.1", "HostPort": "8443" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.5" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].host_addr.port(), 8443);
    }

    #[test]
    fn parse_port_mappings_malformed_port_key_skipped() {
        let inspect = make_inspect(
            r#"{
                "Id": "bad-key-id",
                "NetworkSettings": {
                    "Ports": {
                        "not-a-port": [
                            { "HostIp": "0.0.0.0", "HostPort": "9" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.6" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_port_mappings_no_host_port_skipped() {
        let inspect = make_inspect(
            r#"{
                "Id": "no-host-port-id",
                "NetworkSettings": {
                    "Ports": {
                        "80/tcp": [
                            { "HostIp": "0.0.0.0" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.7" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_port_mappings_no_ports_returns_empty() {
        let inspect = make_inspect(INSPECT_EMPTY_FIELDS);

        let mappings = parse_port_mappings(&inspect);

        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_port_mappings_no_container_ip_returns_empty() {
        let inspect = make_inspect(
            r#"{
                "Id": "no-ip-id",
                "NetworkSettings": {
                    "Ports": {
                        "80/tcp": [
                            { "HostIp": "0.0.0.0", "HostPort": "8080" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_port_mappings_no_network_settings_returns_empty() {
        let inspect = make_inspect(INSPECT_NO_NETWORK_SETTINGS);

        let mappings = parse_port_mappings(&inspect);

        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_port_mappings_uppercase_proto() {
        let inspect = make_inspect(
            r#"{
                "Id": "upper-proto-id",
                "NetworkSettings": {
                    "Ports": {
                        "443/TCP": [
                            { "HostIp": "127.0.0.1", "HostPort": "8443" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.8" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].proto, TransportProtocol::Tcp);
        assert_eq!(mappings[0].host_addr.port(), 8443);
        assert_eq!(mappings[0].container_addr.port(), 443);
    }

    #[test]
    fn parse_port_mappings_invalid_proto_skipped() {
        let inspect = make_inspect(
            r#"{
                "Id": "bad-proto-id",
                "NetworkSettings": {
                    "Ports": {
                        "80/xyz": [
                            { "HostIp": "0.0.0.0", "HostPort": "8080" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.9" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_port_mappings_invalid_container_port_skipped() {
        let inspect = make_inspect(
            r#"{
                "Id": "bad-port-id",
                "NetworkSettings": {
                    "Ports": {
                        "abc/tcp": [
                            { "HostIp": "0.0.0.0", "HostPort": "8080" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.10" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_port_mappings_missing_slash_skipped() {
        let inspect = make_inspect(
            r#"{
                "Id": "no-slash-id",
                "NetworkSettings": {
                    "Ports": {
                        "80": [
                            { "HostIp": "0.0.0.0", "HostPort": "8080" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.11" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_port_mappings_container_port_zero() {
        let inspect = make_inspect(
            r#"{
                "Id": "port-zero-id",
                "NetworkSettings": {
                    "Ports": {
                        "0/udp": [
                            { "HostIp": "127.0.0.1", "HostPort": "5000" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.12" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].container_addr.port(), 0);
        assert_eq!(mappings[0].host_addr.port(), 5000);
        assert_eq!(mappings[0].proto, TransportProtocol::Udp);
    }

    #[test]
    fn parse_port_mappings_max_container_port() {
        let inspect = make_inspect(
            r#"{
                "Id": "max-port-id",
                "NetworkSettings": {
                    "Ports": {
                        "65535/tcp": [
                            { "HostIp": "127.0.0.1", "HostPort": "65535" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.13" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].container_addr.port(), 65535);
        assert_eq!(mappings[0].host_addr.port(), 65535);
        assert_eq!(mappings[0].proto, TransportProtocol::Tcp);
    }

    #[test]
    fn parse_port_mappings_host_port_zero() {
        let inspect = make_inspect(
            r#"{
                "Id": "host-zero-id",
                "NetworkSettings": {
                    "Ports": {
                        "80/tcp": [
                            { "HostIp": "127.0.0.1", "HostPort": "0" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.14" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 1);
        assert_eq!(
            mappings[0].host_addr,
            SocketAddr::new(IpAddr::from_str("127.0.0.1").unwrap(), 0)
        );
    }

    #[test]
    fn parse_port_mappings_host_port_range_from_zero() {
        let inspect = make_inspect(
            r#"{
                "Id": "host-range-zero-id",
                "NetworkSettings": {
                    "Ports": {
                        "80/tcp": [
                            { "HostIp": "127.0.0.1", "HostPort": "0-1023" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.15" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].host_addr.port(), 0);
    }

    #[test]
    fn parse_port_mappings_invalid_host_port_skipped() {
        let inspect = make_inspect(
            r#"{
                "Id": "bad-host-port-id",
                "NetworkSettings": {
                    "Ports": {
                        "80/tcp": [
                            { "HostIp": "0.0.0.0", "HostPort": "abc" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.16" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_port_mappings_empty_host_ip_returns_v4_and_v6() {
        let inspect = make_inspect(
            r#"{
                "Id": "empty-host-ip-id",
                "NetworkSettings": {
                    "Ports": {
                        "80/tcp": [
                            { "HostIp": "", "HostPort": "8080" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.17" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 2);
        assert_eq!(
            mappings[0].host_addr,
            SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), 8080)
        );
        assert_eq!(
            mappings[1].host_addr,
            SocketAddr::new(IpAddr::from_str("::").unwrap(), 8080)
        );
    }

    #[test]
    fn parse_port_mappings_specific_ipv6_host_single() {
        let inspect = make_inspect(
            r#"{
                "Id": "v6-host-id",
                "NetworkSettings": {
                    "Ports": {
                        "80/tcp": [
                            { "HostIp": "::1", "HostPort": "8080" }
                        ]
                    },
                    "Networks": {
                        "bridge": { "IPAddress": "172.17.0.18" }
                    }
                }
            }"#,
        );

        let mappings = parse_port_mappings(&inspect);

        assert_eq!(mappings.len(), 1);
        assert_eq!(
            mappings[0].host_addr,
            SocketAddr::new(IpAddr::from_str("::1").unwrap(), 8080)
        );
        assert_eq!(mappings[0].container_addr.port(), 80);
    }
}
