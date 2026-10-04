//! Shared utilities and types for the `lab-ops` workspace.
//!
//! Provides the canonical [`TransportProtocol`] enum, Docker client helpers,
//! and shared constants used by both `natmap` and `auto-discover`.

/// Shared constants: the natmap socket path and the `lab-ops` binary path.
pub mod consts;
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
