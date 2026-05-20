//! Port allocation and assignment persistence.
//!
//! Allocates ephemeral ports from the range 32768-61000 for container
//! port mappings when no static forwarding port is configured. Assignments
//! are persisted to `ports.json` for crash recovery.

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

const PORT_RANGE_START: u16 = 32768;
const PORT_RANGE_END: u16 = 61000;

/// Persistent mapping of service keys to allocated host ports.
///
/// Keys follow the format `"{service_name}-{container_port}"` (e.g. `"example-drive-80"`).
/// Loaded from and saved to `ports.json` in the state directory.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PortAssignments {
    assignments: HashMap<String, u16>,
}

impl PortAssignments {
    /// Load assignments from a JSON file. Returns empty defaults if the
    /// file does not exist or is unreadable.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Persist assignments to a JSON file. Creates parent directories as
    /// needed.
    pub fn save(&self, path: &Path) -> Result<(), std::io::Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = serde_json::to_string_pretty(self)?;
        std::fs::write(path, contents)
    }

    /// Look up a port assignment by key. Returns `None` if not assigned.
    pub fn get(&self, key: &str) -> Option<u16> {
        self.assignments.get(key).copied()
    }

    /// Assign a port for the given key.
    pub fn set(&mut self, key: String, port: u16) {
        self.assignments.insert(key, port);
    }

    /// Remove an assignment by key. Returns the previously assigned port,
    /// or `None`.
    #[allow(dead_code)]
    pub fn remove(&mut self, key: &str) -> Option<u16> {
        self.assignments.remove(key)
    }

    /// Returns `true` if the given port is already assigned.
    pub fn is_used(&self, port: u16) -> bool {
        self.assignments.values().any(|&p| p == port)
    }
}

/// Find the first free port in the ephemeral range (32768-61000) that is
/// not already assigned and not bound by another process on `0.0.0.0`.
pub fn allocate_port(assignments: &PortAssignments) -> Option<u16> {
    (PORT_RANGE_START..=PORT_RANGE_END)
        .find(|&port| !assignments.is_used(port) && port_is_free("0.0.0.0", port))
}

/// Check whether a TCP port is free by attempting a `TcpListener::bind`.
pub fn port_is_free(host: &str, port: u16) -> bool {
    let addr = format!("{host}:{port}");
    TcpListener::bind(&addr).is_ok()
}

/// Default state directory path: `/var/lib/auto-discover`.
#[allow(dead_code)]
pub fn default_state_dir() -> PathBuf {
    PathBuf::from("/var/lib/auto-discover")
}

/// Default port assignments file: `$state_dir/ports.json`.
#[allow(dead_code)]
pub fn port_assignments_path() -> PathBuf {
    default_state_dir().join("ports.json")
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_set_and_get() {
        let mut pa = PortAssignments::default();
        pa.set("example-drive-80".into(), 32000);
        assert_eq!(pa.get("example-drive-80"), Some(32000));
    }

    #[test]
    fn test_remove() {
        let mut pa = PortAssignments::default();
        pa.set("key".into(), 32000);
        assert_eq!(pa.remove("key"), Some(32000));
        assert_eq!(pa.get("key"), None);
    }

    #[test]
    fn test_persistence() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ports.json");

        let mut pa = PortAssignments::default();
        pa.set("s1".into(), 40000);
        pa.set("s2".into(), 40001);
        pa.save(&path).unwrap();

        let loaded = PortAssignments::load(&path);
        assert_eq!(loaded.get("s1"), Some(40000));
        assert_eq!(loaded.get("s2"), Some(40001));
    }

    #[test]
    fn test_port_is_free_localhost() {
        let port = 32768;
        let result = port_is_free("127.0.0.1", port);
        assert!(result);
    }

    #[test]
    fn test_port_is_free_occupied() {
        let port = 32769;
        let addr = format!("127.0.0.1:{port}");
        let listener = TcpListener::bind(&addr).unwrap();
        assert!(!port_is_free("127.0.0.1", port));
        drop(listener);
        assert!(port_is_free("127.0.0.1", port));
    }

    #[test]
    fn test_allocate_port_assigns_unique() {
        let mut pa = PortAssignments::default();
        let p1 = allocate_port(&pa).unwrap();
        pa.set("s1".into(), p1);
        let p2 = allocate_port(&pa).unwrap();
        assert_ne!(p1, p2);
    }
}
