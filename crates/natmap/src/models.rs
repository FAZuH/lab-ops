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

// --- API request types ---

/// JSON body for creating or deleting a DNAT rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnatRequest {
    pub ext_ip: String,
    pub int_ip: String,
    pub ports: String,
    pub proto: TransportProtocol,
    pub ext_if: Option<String>,
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
}

/// Response returned by the `GET /mappings` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListResponse {
    pub docker: Vec<DockerPortMap>,
    pub dnats: Vec<DnatConfig>,
    pub snats: Vec<SnatConfig>,
    pub hairpins: Vec<HairpinConfig>,
}
