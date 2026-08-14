//! Shared utilities and types for the `lab-ops` workspace.
//!
//! Provides the canonical [`TransportProtocol`] enum, Docker client helpers,
//! and shared constants used by both `natmap` and `auto-discover`.

pub mod consts;
pub mod docker;
pub mod port;
pub mod protocol;

pub use consts::NATMAP_SOCKET;
pub use docker::ContainerInfo;
pub use docker::ContainerNetwork;
pub use docker::DockerClient;
pub use docker::PortMapping;
pub use docker::connect;
pub use docker::parse_container_inspect;
pub use docker::parse_port_mappings;
pub use docker::trim_container_name;
pub use port::PortAllocator;
pub use port::PortAssignments;
pub use protocol::TransportProtocol;
