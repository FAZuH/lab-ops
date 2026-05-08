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

/// Mirrors PortBindingReq — desired mapping configuration
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMappingRequest {
    pub host_addr: SocketAddr,      // 0.0.0.0:8080 or [::]:8080
    pub container_addr: SocketAddr, // 172.17.0.2:80
    pub proto: TransportProtocol,
}

impl PortMappingRequest {
    pub fn is_ipv6(&self) -> bool {
        self.host_addr.is_ipv6()
    }
}

/// Mirrors PortBinding — an active, installed mapping
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivePortMapping {
    pub id: u64,
    pub request: PortMappingRequest,
    pub container_id: String,
    pub container_name: String,
    pub rule_comment: String, // iptables comment for idempotent existence checks
}

impl ActivePortMapping {
    pub fn new(
        id: u64,
        request: PortMappingRequest,
        container_id: String,
        container_name: String,
    ) -> Self {
        let rule_comment = format!("dockernatmap:{}:{}", container_id, request.host_addr.port());
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
