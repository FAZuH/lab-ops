//! Port utilities and management.
//!
//! Three layers of port management:
//! - [`create_freebind_socket`] / [`is_port_free`] — Low-level port checking
//! - [`PortAllocator`] — Runtime TCP pre-bind reservation for conflict prevention
//! - [`PortAssignments`] / [`allocate_port`] — Persistent ephemeral port allocation

use std::collections::HashMap;
use std::net::SocketAddr;
use std::net::ToSocketAddrs;
use std::path::Path;

use color_eyre::Result;
use color_eyre::eyre::eyre;
use serde::Deserialize;
use serde::Serialize;
use socket2::Domain;
use socket2::Socket;
use socket2::Type;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::info;

const PORT_RANGE_START: u16 = 32768;
const PORT_RANGE_END: u16 = 61000;

// ---------------------------------------------------------------------------
// Low-level socket utilities
// ---------------------------------------------------------------------------

/// Creates and configures a `Socket` for `addr` with `SO_REUSEADDR`
/// and the appropriate `IP_FREEBIND` option.
pub fn create_freebind_socket(addr: &SocketAddr) -> std::io::Result<Socket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };

    let socket = Socket::new(domain, Type::STREAM, None)?;

    // Without this we may fail to bind if the socket was just released
    // and the port is briefly in TIME_WAIT.
    socket.set_reuse_address(true)?;

    if addr.is_ipv4() {
        socket.set_freebind_v4(true)?;
    } else {
        socket.set_freebind_v6(true)?;
    }

    Ok(socket)
}

/// Checks if a TCP port is free by attempting to bind to it using a socket
/// configured with `SO_REUSEADDR` and `IP_FREEBIND`. This is more robust
/// than a simple `TcpListener::bind` as it handles `TIME_WAIT` states gracefully.
pub fn is_port_free<A: ToSocketAddrs>(addr: A) -> bool {
    let Ok(mut addrs) = addr.to_socket_addrs() else {
        return false;
    };

    let Some(sock_addr) = addrs.next() else {
        return false;
    };

    let Ok(socket) = create_freebind_socket(&sock_addr) else {
        return false;
    };

    socket.bind(&sock_addr.into()).is_ok()
}

// ---------------------------------------------------------------------------
// PortAllocator — runtime TCP pre-bind reservation
// ---------------------------------------------------------------------------

/// A concurrency-safe port reservation system backed by TCP pre-bind.
///
/// Ports are keyed by [`SocketAddr`]. Allocating a port binds a
/// [`TcpListener`] to it, preventing other processes or concurrent daemon
/// operations from claiming the same port.
pub struct PortAllocator {
    sockets: RwLock<HashMap<SocketAddr, TcpListener>>,
}

impl Default for PortAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl PortAllocator {
    pub fn new() -> Self {
        Self {
            sockets: RwLock::new(HashMap::new()),
        }
    }

    /// Reserves `addr` by binding a [`TcpListener`] to it.
    ///
    /// # Errors
    ///
    /// Returns an error if the port is already bound by another process.
    pub async fn allocate(&self, addr: SocketAddr) -> Result<()> {
        let socket = create_freebind_socket(&addr)
            .map_err(|e| eyre!("Failed to create socket for {addr}: {e}"))?;

        socket
            .bind(&addr.into())
            .map_err(|e| eyre!("Failed to reserve {addr}: {e}"))?;

        socket
            .listen(128)
            .map_err(|e| eyre!("Failed to listen on {addr}: {e}"))?;

        let std_listener: std::net::TcpListener = socket.into();
        std_listener
            .set_nonblocking(true)
            .map_err(|e| eyre!("Failed to set nonblocking for {addr}: {e}"))?;

        let listener = TcpListener::from_std(std_listener)
            .map_err(|e| eyre!("Failed to create tokio listener for {addr}: {e}"))?;

        info!("Reserved {addr}");
        self.sockets.write().await.insert(addr, listener);
        Ok(())
    }

    /// Releases the reservation for `addr`, if any.
    pub async fn deallocate(&self, addr: SocketAddr) {
        self.sockets.write().await.remove(&addr);
        info!("Released {addr}");
    }

    /// Returns `true` if `addr` has an active reservation.
    pub async fn is_allocated(&self, addr: SocketAddr) -> bool {
        if self.sockets.read().await.contains_key(&addr) {
            return true;
        }

        let Ok(socket) = create_freebind_socket(&addr) else {
            return false;
        };

        match socket.bind(&addr.into()) {
            Err(e) => e.kind() == std::io::ErrorKind::AddrInUse,
            Ok(_) => false,
        }
    }

    /// Releases all active reservations.
    pub async fn deallocate_all(&self) {
        let mut sockets = self.sockets.write().await;
        let count = sockets.len();
        sockets.clear();
        info!("Released all {count} reservations");
    }
}

// ---------------------------------------------------------------------------
// PortAssignments — persistent ephemeral port allocation
// ---------------------------------------------------------------------------

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

    /// Look up an existing assignment, or allocate a free port and assign it.
    ///
    /// Returns `None` if no port is available in the ephemeral range.
    pub fn get_or_allocate(&mut self, key: &str) -> Option<u16> {
        if let Some(&p) = self.assignments.get(key) {
            return Some(p);
        }
        let p = self.allocate_port()?;
        self.assignments.insert(key.to_string(), p);
        Some(p)
    }

    fn allocate_port(&self) -> Option<u16> {
        (PORT_RANGE_START..=PORT_RANGE_END)
            .find(|&port| !self.is_used(port) && is_port_free(format!("0.0.0.0:{port}")))
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn set_and_get() {
        let mut pa = PortAssignments::default();
        pa.set("example-drive-80".into(), 32000);
        assert_eq!(pa.get("example-drive-80"), Some(32000));
    }

    #[test]
    fn remove() {
        let mut pa = PortAssignments::default();
        pa.set("key".into(), 32000);
        assert_eq!(pa.remove("key"), Some(32000));
        assert_eq!(pa.get("key"), None);
    }

    #[test]
    fn persistence() {
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
    fn get_or_allocate_returns_existing() {
        let mut pa = PortAssignments::default();
        pa.set("key".into(), 32000);
        assert_eq!(pa.get_or_allocate("key"), Some(32000));
    }

    #[test]
    fn get_or_allocate_creates_new() {
        let mut pa = PortAssignments::default();
        let p = pa.get_or_allocate("new-key").unwrap();
        assert!((PORT_RANGE_START..=PORT_RANGE_END).contains(&p));
        assert_eq!(pa.get("new-key"), Some(p));
    }

    #[test]
    fn is_port_free_localhost() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        assert!(is_port_free(format!("127.0.0.1:{port}")));
    }

    #[test]
    fn is_port_free_occupied() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!is_port_free(format!("127.0.0.1:{port}")));
        drop(listener);
        assert!(is_port_free(format!("127.0.0.1:{port}")));
    }

    #[test]
    fn allocate_port_assigns_unique() {
        let mut pa = PortAssignments::default();
        let p1 = pa.allocate_port().unwrap();
        pa.set("s1".into(), p1);
        let p2 = pa.allocate_port().unwrap();
        assert_ne!(p1, p2);
    }
}
