//! Shared constants for the lab-ops workspace.

/// Default path to the natmap daemon Unix socket.
pub const NATMAP_SOCKET: &str = "/run/natmap.sock";

/// The lab-ops binary installed on the host system.
pub const LAB_OPS_BIN: &str = "/usr/local/bin/lab-ops";

/// The lab-ops CLI command name (for subprocess invocations).
pub const LAB_OPS_CMD: &str = "lab-ops";
