//! Shared utilities and types for the `lab-ops` workspace.
//!
//! Provides the canonical [`TransportProtocol`] enum, the `discovery.yaml`
//! declaration model, Docker client helpers, and shared constants used by
//! `natmap` and the root binary.

/// Shared constants: the natmap socket path and the `lab-ops` binary path.
pub mod consts;
/// The `discovery.yaml` declaration model: parsing, defaults, and service resolution.
pub mod discovery;
/// Docker client helpers, container inspection, and the shared container shapes.
pub mod docker;
/// Port reservation ([`PortAllocator`]) and freebind socket creation.
pub mod port;
/// The canonical [`TransportProtocol`] enum shared by every crate.
pub mod protocol;

/// The natmap socket path, re-exported so callers need only this crate root.
pub use consts::NATMAP_SOCKET;
/// The canonical [`TransportProtocol`], re-exported so callers need only this
/// crate root.
pub use protocol::TransportProtocol;
