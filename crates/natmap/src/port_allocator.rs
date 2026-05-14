//! Port conflict prevention via TCP pre-bind.
//!
//! [`PortAllocator`] temporarily binds to a port using `SO_REUSEADDR` before
//! the iptables rule is installed. If the bind succeeds, the port is available.
//! The listener is held for the lifetime of the mapping to prevent conflicts.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;

use color_eyre::Result;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::info;

/// A concurrency-safe port reservation system backed by TCP pre-bind.
///
/// Ports are keyed by `"<ip>:<port>"` strings. Allocating a port binds a
/// [`TcpListener`] to it, preventing other processes or concurrent daemon
/// operations from claiming the same port.
pub struct PortAllocator {
    sockets: RwLock<HashMap<String, TcpListener>>,
}

impl Default for PortAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl PortAllocator {
    /// Creates a new empty [`PortAllocator`].
    pub fn new() -> Self {
        Self {
            sockets: RwLock::new(HashMap::new()),
        }
    }

    /// Reserves a port by binding a [`TcpListener`] to `addr`.
    ///
    /// The `key` is a unique identifier (conventionally `"<ip>:<port>"`)
    /// used for later deallocation.
    ///
    /// # Errors
    ///
    /// Returns an error if the port is already bound by another process.
    pub async fn allocate(&self, key: &str, addr: SocketAddr) -> Result<()> {
        let bind_addr = SocketAddrV4::new(Ipv4Addr::new(0, 0, 0, 0), addr.port());
        let listener = TcpListener::bind(bind_addr)
            .await
            .map_err(|e| color_eyre::eyre::eyre!("Port {} already in use: {}", addr.port(), e))?;
        info!("Port {} reserved for {}", addr.port(), key);
        self.sockets.write().await.insert(key.to_string(), listener);
        Ok(())
    }

    /// Releases a previously allocated port by removing its listener.
    pub async fn deallocate(&self, key: &str) {
        self.sockets.write().await.remove(key);
        info!("Port released for {}", key);
    }

    /// Checks whether a given key has an active port reservation.
    pub async fn is_allocated(&self, key: &str) -> bool {
        self.sockets.read().await.contains_key(key)
    }

    /// Releases all port reservations.
    pub async fn deallocate_all(&self) {
        let count = self.sockets.write().await.len();
        self.sockets.write().await.clear();
        info!("Released all {} port reservations", count);
    }
}
