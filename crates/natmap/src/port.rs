//! Port conflict prevention via TCP pre-bind.
//!
//! [`PortAllocator`] temporarily binds to a port using `SO_REUSEADDR` before
//! the nftables rule is installed. If the bind succeeds, the port is available.
//! The listener is held for the lifetime of the mapping to prevent conflicts.

use std::collections::HashMap;
use std::net::SocketAddr;

use color_eyre::Result;
use color_eyre::eyre::eyre;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::info;

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
        let socket = lab_lib::port::create_freebind_socket(&addr)
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

        let Ok(socket) = lab_lib::port::create_freebind_socket(&addr) else {
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
