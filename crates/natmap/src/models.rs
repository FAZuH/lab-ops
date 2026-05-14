//! Data models for the natmap daemon and its API.
//!
//! Defines request/response types, persisted state structures, and shared
//! enums used across the CLI, daemon, and iptables modules.

use std::collections::HashMap;
use std::fmt::Display;
use std::net::SocketAddr;

use serde::Deserialize;
use serde::Serialize;

/// Transport protocol (TCP or UDP).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum TransportProtocol {
    Tcp,
    Udp,
}

impl Display for TransportProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Tcp => write!(f, "tcp"),
            Self::Udp => write!(f, "udp"),
        }
    }
}

/// Describes the desired port mapping between a host and a container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMappingRequest {
    pub host_addr: SocketAddr,
    pub container_addr: SocketAddr,
    pub proto: TransportProtocol,
}

impl PortMappingRequest {
    /// Returns whether the host address is an IPv6 address.
    pub fn is_ipv6(&self) -> bool {
        self.host_addr.is_ipv6()
    }
}

/// An active port mapping that has been installed in iptables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivePortMapping {
    /// Unique numeric ID assigned by the daemon.
    pub id: u64,
    /// The mapping request that was fulfilled.
    pub request: PortMappingRequest,
    /// Docker container ID.
    pub container_id: String,
    /// Human-readable container name.
    pub container_name: String,
    /// iptables comment used to identify this mapping's rules.
    pub rule_comment: String,
}

impl ActivePortMapping {
    /// Creates a new [`ActivePortMapping`] with a generated rule comment.
    ///
    /// The comment format is `natmap:<container_id>:<host_port>`.
    pub fn new(
        id: u64,
        request: PortMappingRequest,
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
pub struct RemapRequest {
    pub host_port: u16,
    pub new_host_port: u16,
}

/// Request to add a new port mapping to a running container.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddMappingRequest {
    /// Host IP to bind to (defaults to `0.0.0.0`).
    #[serde(default = "default_host_ip")]
    pub host_ip: String,
    /// Port on the host.
    pub host_port: u16,
    /// Port inside the container.
    pub container_port: u16,
    /// Transport protocol (`tcp` or `udp`, defaults to `tcp`).
    #[serde(default = "default_proto")]
    pub proto: String,
}

fn default_host_ip() -> String {
    "0.0.0.0".to_string()
}

fn default_proto() -> String {
    "tcp".to_string()
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
    /// Transport protocol (`tcp` or `udp`).
    pub proto: String,
    /// Optional external network interface.
    pub ext_if: Option<String>,
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

/// A static hairpin NAT rule configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HairpinConfig {
    /// External IP address.
    pub ext_ip: String,
    /// Internal IP address.
    pub int_ip: String,
    /// Comma-separated list of ports.
    pub ports: String,
    /// Transport protocol (`tcp` or `udp`).
    pub proto: String,
}

// --- API request types ---

/// JSON body for creating or deleting a DNAT rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnatRequest {
    pub ext_ip: String,
    pub int_ip: String,
    pub ports: String,
    #[serde(default = "default_proto")]
    pub proto: String,
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
    #[serde(default = "default_proto")]
    pub proto: String,
}

// --- Persisted daemon state ---

/// The complete persisted state of the natmap daemon.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DaemonState {
    /// Docker container mappings, keyed by container ID.
    pub docker: HashMap<String, Vec<ActivePortMapping>>,
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
    pub docker: Vec<ActivePortMapping>,
    pub dnats: Vec<DnatConfig>,
    pub snats: Vec<SnatConfig>,
    pub hairpins: Vec<HairpinConfig>,
}
