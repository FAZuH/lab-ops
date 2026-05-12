use std::collections::HashMap;
use std::fmt::Display;
use std::net::SocketAddr;

use serde::Deserialize;
use serde::Serialize;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMappingRequest {
    pub host_addr: SocketAddr,
    pub container_addr: SocketAddr,
    pub proto: TransportProtocol,
}

impl PortMappingRequest {
    pub fn is_ipv6(&self) -> bool {
        self.host_addr.is_ipv6()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivePortMapping {
    pub id: u64,
    pub request: PortMappingRequest,
    pub container_id: String,
    pub container_name: String,
    pub rule_comment: String,
}

impl ActivePortMapping {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemapRequest {
    pub host_port: u16,
    pub new_host_port: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddMappingRequest {
    #[serde(default = "default_host_ip")]
    pub host_ip: String,
    pub host_port: u16,
    pub container_port: u16,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DnatConfig {
    pub ext_ip: String,
    pub int_ip: String,
    pub ports: String,
    pub proto: String,
    pub ext_if: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnatConfig {
    pub int_ip: String,
    pub ext_ip: String,
    pub ext_if: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HairpinConfig {
    pub ext_ip: String,
    pub int_ip: String,
    pub ports: String,
    pub proto: String,
}

// --- API request types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DnatRequest {
    pub ext_ip: String,
    pub int_ip: String,
    pub ports: String,
    #[serde(default = "default_proto")]
    pub proto: String,
    pub ext_if: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnatRequest {
    pub int_ip: String,
    pub ext_ip: String,
    pub ext_if: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HairpinRequest {
    pub ext_ip: String,
    pub int_ip: String,
    pub ports: String,
    #[serde(default = "default_proto")]
    pub proto: String,
}

// --- Persisted daemon state ---

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DaemonState {
    pub docker: HashMap<String, Vec<ActivePortMapping>>,
    pub dnats: Vec<DnatConfig>,
    pub snats: Vec<SnatConfig>,
    pub hairpins: Vec<HairpinConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListResponse {
    pub docker: Vec<ActivePortMapping>,
    pub dnats: Vec<DnatConfig>,
    pub snats: Vec<SnatConfig>,
    pub hairpins: Vec<HairpinConfig>,
}
