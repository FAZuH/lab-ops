//! Compile-time constants for the natmap daemon paths and package name.

/// Default path to natmap's Unix socket.
pub const DAEMON_SOCK: &str = "/run/natmap.sock";

/// Default path to natmap state.json file
pub const STATE: &str = "/var/lib/natmap/state.json";

/// Crate name
pub const PKG_NAME: &str = "natmap";
