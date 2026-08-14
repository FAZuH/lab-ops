//! Data models for the natmap daemon and its API.
//!
//! Defines request/response types, persisted state structures, and shared
//! enums used across the CLI, daemon, and iptables modules.

use std::collections::HashMap;
use std::net::SocketAddr;

pub use lab_ops_lab_lib::TransportProtocol;
use serde::Deserialize;
use serde::Serialize;

/// Describes the desired port mapping between a host and a container.
///
/// ```
/// use std::net::{IpAddr, SocketAddr};
/// use std::str::FromStr;
/// use lab_ops_natmap::models::{DockerPortMapRequest, TransportProtocol};
///
/// let req = DockerPortMapRequest {
///     host_addr: SocketAddr::new(IpAddr::from_str("::").unwrap(), 80),
///     container_addr: SocketAddr::new(IpAddr::from_str("::1").unwrap(), 80),
///     proto: TransportProtocol::Tcp,
/// };
/// assert!(req.is_ipv6());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerPortMapRequest {
    pub host_addr: SocketAddr,
    pub container_addr: SocketAddr,
    pub proto: TransportProtocol,
}

impl DockerPortMapRequest {
    /// Returns whether the host address is an IPv6 address.
    pub fn is_ipv6(&self) -> bool {
        self.host_addr.is_ipv6()
    }
}

/// An active port mapping that has been installed in iptables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerPortMap {
    /// Unique numeric ID assigned by the daemon.
    pub id: u64,
    /// The mapping request that was fulfilled.
    pub request: DockerPortMapRequest,
    /// Docker container ID.
    pub container_id: String,
    /// Docker container name.
    pub container_name: String,
    /// iptables comment used to identify this mapping's rules.
    pub rule_comment: String,
}

impl DockerPortMap {
    /// Creates a new [`DockerPortMap`] with a generated rule comment.
    ///
    /// The comment format is `natmap:<container_id>:<host_port>`.
    ///
    /// ```
    /// use std::net::{IpAddr, SocketAddr};
    /// use std::str::FromStr;
    /// use lab_ops_natmap::models::{DockerPortMap, DockerPortMapRequest, TransportProtocol};
    ///
    /// let req = DockerPortMapRequest {
    ///     host_addr: SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), 8080),
    ///     container_addr: SocketAddr::new(IpAddr::from_str("172.17.0.2").unwrap(), 80),
    ///     proto: TransportProtocol::Tcp,
    /// };
    /// let m = DockerPortMap::new(1, req, "abc123".into(), "my-nginx".into());
    /// assert_eq!(m.rule_comment, "natmap:abc123:8080");
    /// assert_eq!(m.container_id, "abc123");
    /// ```
    pub fn new(
        id: u64,
        request: DockerPortMapRequest,
        container_id: String,
        container_name: String,
    ) -> Self {
        let rule_comment = format!("natmap:{}:{}", container_id, request.host_addr.port());
        Self {
            id,
            request,
            container_id,
            container_name,
            rule_comment,
        }
    }
}

/// Request to remap a host port for an existing container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DockerRemapRequest {
    pub host_port: u16,
    pub new_host_port: u16,
}

/// Request to add a new port mapping.
///
/// For Docker containers, `container_id` in the URL path identifies the container
/// and its IP is resolved via `docker inspect`. For local (non-Docker) services,
/// set `target_ip` to skip Docker inspection entirely.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DockerAddMapRequest {
    /// Host IP to bind to (defaults to `0.0.0.0`).
    #[serde(default = "default_host_ip")]
    pub host_ip: String,
    /// Port on the host.
    pub host_port: u16,
    /// Port on the target (container or local service).
    pub container_port: u16,
    /// Optional target IP override. When set, skips Docker inspect and uses
    /// this IP directly — useful for local (non-Docker) services.
    #[serde(default)]
    pub target_ip: Option<String>,
    /// Transport protocol (`tcp` or `udp`, defaults to `tcp`).
    #[serde(default = "default_proto")]
    pub proto: TransportProtocol,
}

fn default_host_ip() -> String {
    "0.0.0.0".to_string()
}

fn default_proto() -> TransportProtocol {
    TransportProtocol::default()
}

// --- Static NAT configs (persisted to state.json) ---

/// A static DNAT (destination NAT) rule configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnatConfig {
    /// External (public) IP address.
    pub ext_ip: String,
    /// Internal (private) destination IP address.
    pub int_ip: String,
    /// Comma-separated list of ports.
    pub ports: String,
    /// Transport protocol.
    pub proto: TransportProtocol,
    /// Optional external network interface.
    pub ext_if: Option<String>,
    /// Preserve the source IP of forwarded traffic (metadata only — the
    /// daemon does not apply a MASQUERADE rule for this mapping).
    #[serde(default)]
    pub preserve_src_ip: bool,
}

impl DnatConfig {
    /// iptables comment used to identify this DNAT rule's rules.
    pub fn rule_comment(&self) -> String {
        format!("natmap:dnat:{}:{}", self.ext_ip, self.ports)
    }
}

/// A static SNAT (source NAT) rule configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnatConfig {
    /// Internal source IP address.
    pub int_ip: String,
    /// External (masquerade) IP address.
    pub ext_ip: String,
    /// External network interface.
    pub ext_if: String,
}

impl SnatConfig {
    /// iptables comment used to identify this SNAT rule's rules.
    pub fn rule_comment(&self) -> String {
        format!("natmap:snat:{}:{}", self.int_ip, self.ext_ip)
    }
}

/// A static hairpin NAT rule configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HairpinConfig {
    /// External IP address.
    pub ext_ip: String,
    /// Internal IP address.
    pub int_ip: String,
    /// Comma-separated list of ports.
    pub ports: String,
    /// Transport protocol.
    pub proto: TransportProtocol,
    /// Optional LAN source CIDR. When set, only traffic from this subnet is
    /// MASQUERADEd (instead of all sources with `0.0.0.0/0`), and the
    /// PREROUTING DNAT rule is skipped. Used for `preserve_src_ip` hairpin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_cidr: Option<String>,
}

impl HairpinConfig {
    /// iptables comment used to identify this hairpin rule's rules.
    pub fn rule_comment(&self) -> String {
        format!(
            "natmap:hairpin:{}:{}:{}",
            self.ext_ip, self.int_ip, self.ports
        )
    }
}

// --- Live rules (daemon-reported) ---

/// The kind of a NAT rule reported by the daemon's live rule listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    /// Static DNAT rule (`natmap:dnat:<ext_ip>:<ports>`).
    Dnat,
    /// Static hairpin rule (`natmap:hairpin:<ext_ip>:<int_ip>:<ports>`).
    Hairpin,
    /// Static SNAT rule (`natmap:snat:<int_ip>:<ext_ip>`).
    Snat,
    /// Docker container port mapping (`natmap:<container>:<port>`).
    Mapping,
}

/// A single NAT rule installed in iptables, as reported by the daemon.
///
/// Parsed from the daemon's live rule listing; the daemon is the authority
/// on what is actually installed. `Ord` is derived so handlers can produce
/// a deterministic, deduplicated listing.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LiveRule {
    /// Kind of rule.
    pub kind: RuleKind,
    /// External IP address.
    pub ext_ip: String,
    /// Internal IP address.
    pub int_ip: String,
    /// Ports matched by the rule (empty for rules without ports).
    pub ports: Vec<u16>,
    /// Transport protocol.
    pub proto: TransportProtocol,
}

// --- API request types ---

/// JSON body for creating or deleting a DNAT rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnatRequest {
    pub ext_ip: String,
    pub int_ip: String,
    pub ports: String,
    pub proto: TransportProtocol,
    pub ext_if: Option<String>,
    #[serde(default)]
    pub preserve_src_ip: bool,
}

/// JSON body for creating or deleting an SNAT rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnatRequest {
    pub int_ip: String,
    pub ext_ip: String,
    pub ext_if: String,
}

/// JSON body for creating or deleting a hairpin rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HairpinRequest {
    pub ext_ip: String,
    pub int_ip: String,
    pub ports: String,
    pub proto: TransportProtocol,
    #[serde(default)]
    pub lan_cidr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRouteConfig {
    pub src_ip: String,
    pub via: String,
    pub table: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRouteRequest {
    pub src_ip: String,
    pub via: String,
    pub table: u32,
}

// --- Config → request conversions ---

impl From<DnatConfig> for DnatRequest {
    fn from(config: DnatConfig) -> Self {
        Self {
            ext_ip: config.ext_ip,
            int_ip: config.int_ip,
            ports: config.ports,
            proto: config.proto,
            ext_if: config.ext_if,
            preserve_src_ip: config.preserve_src_ip,
        }
    }
}

impl From<SnatConfig> for SnatRequest {
    fn from(config: SnatConfig) -> Self {
        Self {
            int_ip: config.int_ip,
            ext_ip: config.ext_ip,
            ext_if: config.ext_if,
        }
    }
}

impl From<HairpinConfig> for HairpinRequest {
    fn from(config: HairpinConfig) -> Self {
        Self {
            ext_ip: config.ext_ip,
            int_ip: config.int_ip,
            ports: config.ports,
            proto: config.proto,
            lan_cidr: config.lan_cidr,
        }
    }
}

impl From<PolicyRouteConfig> for PolicyRouteRequest {
    fn from(config: PolicyRouteConfig) -> Self {
        Self {
            src_ip: config.src_ip,
            via: config.via,
            table: config.table,
        }
    }
}

// --- Persisted daemon state ---

/// The complete persisted state of the natmap daemon.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DaemonState {
    /// Docker container port mappings, keyed by container ID.
    pub mapping: HashMap<String, Vec<DockerPortMap>>,
    /// Static DNAT rule configurations.
    pub dnats: Vec<DnatConfig>,
    /// Static SNAT rule configurations.
    pub snats: Vec<SnatConfig>,
    /// Static hairpin rule configurations.
    pub hairpins: Vec<HairpinConfig>,
    /// Static policy routing configurations.
    #[serde(default)]
    pub policy_routes: Vec<PolicyRouteConfig>,
}

/// Response returned by the `GET /mappings` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListResponse {
    pub docker: Vec<DockerPortMap>,
    pub dnats: Vec<DnatConfig>,
    pub snats: Vec<SnatConfig>,
    pub hairpins: Vec<HairpinConfig>,
    pub policy_routes: Vec<PolicyRouteConfig>,
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::net::SocketAddr;
    use std::str::FromStr;

    use super::*;

    // ── DockerPortMapRequest::is_ipv6 ──

    #[test]
    fn is_ipv6_ipv4_returns_false() {
        let req = DockerPortMapRequest {
            host_addr: SocketAddr::new(IpAddr::from_str("192.168.1.1").unwrap(), 80),
            container_addr: SocketAddr::new(IpAddr::from_str("10.0.0.1").unwrap(), 80),
            proto: TransportProtocol::Tcp,
        };
        assert!(!req.is_ipv6());
    }

    #[test]
    fn is_ipv6_ipv6_returns_true() {
        let req = DockerPortMapRequest {
            host_addr: SocketAddr::new(IpAddr::from_str("2001:db8::1").unwrap(), 80),
            container_addr: SocketAddr::new(IpAddr::from_str("::1").unwrap(), 80),
            proto: TransportProtocol::Tcp,
        };
        assert!(req.is_ipv6());
    }

    #[test]
    fn is_ipv6_unspecified_ipv4_returns_false() {
        let req = DockerPortMapRequest {
            host_addr: SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), 80),
            container_addr: SocketAddr::new(IpAddr::from_str("10.0.0.1").unwrap(), 80),
            proto: TransportProtocol::Udp,
        };
        assert!(!req.is_ipv6());
    }

    #[test]
    fn is_ipv6_unspecified_ipv6_returns_true() {
        let req = DockerPortMapRequest {
            host_addr: SocketAddr::new(IpAddr::from_str("::").unwrap(), 80),
            container_addr: SocketAddr::new(IpAddr::from_str("::1").unwrap(), 80),
            proto: TransportProtocol::Tcp,
        };
        assert!(req.is_ipv6());
    }

    // ── DockerPortMap::new ──

    #[test]
    fn new_docker_port_map_comment_format() {
        let req = DockerPortMapRequest {
            host_addr: SocketAddr::new(IpAddr::from_str("0.0.0.0").unwrap(), 8080),
            container_addr: SocketAddr::new(IpAddr::from_str("172.17.0.2").unwrap(), 80),
            proto: TransportProtocol::Tcp,
        };
        let m = DockerPortMap::new(1, req, "abc123".into(), "my-nginx".into());
        assert_eq!(m.id, 1);
        assert_eq!(m.rule_comment, "natmap:abc123:8080");
        assert_eq!(m.container_id, "abc123");
        assert_eq!(m.container_name, "my-nginx");
    }

    #[test]
    fn new_docker_port_map_ipv6_comment() {
        let req = DockerPortMapRequest {
            host_addr: SocketAddr::new(IpAddr::from_str("::").unwrap(), 443),
            container_addr: SocketAddr::new(IpAddr::from_str("::1").unwrap(), 443),
            proto: TransportProtocol::Tcp,
        };
        let m = DockerPortMap::new(42, req, "container-1".into(), "test-svc".into());
        assert_eq!(m.id, 42);
        assert_eq!(m.rule_comment, "natmap:container-1:443");
    }

    #[test]
    fn new_docker_port_map_zero_id() {
        let req = DockerPortMapRequest {
            host_addr: SocketAddr::new(IpAddr::from_str("10.0.0.1").unwrap(), 0),
            container_addr: SocketAddr::new(IpAddr::from_str("10.0.0.2").unwrap(), 0),
            proto: TransportProtocol::Tcp,
        };
        let m = DockerPortMap::new(0, req, "id-zero".into(), "zero-port".into());
        assert_eq!(m.id, 0);
        assert_eq!(m.rule_comment, "natmap:id-zero:0");
    }

    // ── DnatConfig::rule_comment ──

    #[test]
    fn dnat_rule_comment_basic() {
        let cfg = DnatConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };
        assert_eq!(cfg.rule_comment(), "natmap:dnat:203.0.113.50:80");
    }

    #[test]
    fn dnat_rule_comment_multiport() {
        let cfg = DnatConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80,443,8080".into(),
            proto: TransportProtocol::Tcp,
            ext_if: None,
            preserve_src_ip: false,
        };
        assert_eq!(cfg.rule_comment(), "natmap:dnat:203.0.113.50:80,443,8080");
    }

    #[test]
    fn dnat_rule_comment_with_ext_if() {
        let cfg = DnatConfig {
            ext_ip: "198.51.100.10".into(),
            int_ip: "10.0.0.1".into(),
            ports: "53".into(),
            proto: TransportProtocol::Udp,
            ext_if: Some("eth0".into()),
            preserve_src_ip: true,
        };
        assert_eq!(cfg.rule_comment(), "natmap:dnat:198.51.100.10:53");
    }

    // ── SnatConfig::rule_comment ──

    #[test]
    fn snat_rule_comment_basic() {
        let cfg = SnatConfig {
            int_ip: "10.0.0.1".into(),
            ext_ip: "203.0.113.50".into(),
            ext_if: "eth0".into(),
        };
        assert_eq!(cfg.rule_comment(), "natmap:snat:10.0.0.1:203.0.113.50");
    }

    #[test]
    fn snat_rule_comment_ipv6() {
        let cfg = SnatConfig {
            int_ip: "2001:db8::1".into(),
            ext_ip: "2001:db8::ff".into(),
            ext_if: "eth0".into(),
        };
        assert_eq!(cfg.rule_comment(), "natmap:snat:2001:db8::1:2001:db8::ff");
    }

    // ── HairpinConfig::rule_comment ──

    #[test]
    fn hairpin_rule_comment_basic() {
        let cfg = HairpinConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80".into(),
            proto: TransportProtocol::Tcp,
            lan_cidr: None,
        };
        assert_eq!(
            cfg.rule_comment(),
            "natmap:hairpin:203.0.113.50:10.0.0.99:80"
        );
    }

    #[test]
    fn hairpin_rule_comment_with_lan_cidr() {
        let cfg = HairpinConfig {
            ext_ip: "203.0.113.50".into(),
            int_ip: "10.0.0.99".into(),
            ports: "80,443".into(),
            proto: TransportProtocol::Udp,
            lan_cidr: Some("10.0.0.0/24".into()),
        };
        assert_eq!(
            cfg.rule_comment(),
            "natmap:hairpin:203.0.113.50:10.0.0.99:80,443"
        );
    }

    #[test]
    fn hairpin_rule_comment_multiport() {
        let cfg = HairpinConfig {
            ext_ip: "198.51.100.10".into(),
            int_ip: "10.0.0.1".into(),
            ports: "3000,3001,3002".into(),
            proto: TransportProtocol::Tcp,
            lan_cidr: None,
        };
        assert_eq!(
            cfg.rule_comment(),
            "natmap:hairpin:198.51.100.10:10.0.0.1:3000,3001,3002"
        );
    }
}
