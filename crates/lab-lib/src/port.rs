//! Port utilities and management.
//!
//! Two layers of port management:
//! - [`create_freebind_socket`] — Low-level freebind socket creation
//! - [`PortAllocator`] — Runtime TCP/UDP pre-bind reservation for conflict prevention

use std::collections::HashMap;
use std::net::SocketAddr;

use color_eyre::Result;
use color_eyre::eyre::eyre;
use socket2::Domain;
use socket2::Socket;
use socket2::Type;
use tokio::net::TcpListener;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tracing::info;

use crate::protocol::TransportProtocol;

// ---------------------------------------------------------------------------
// Low-level socket utilities
// ---------------------------------------------------------------------------

/// Creates and configures a `Socket` for `addr` with `SO_REUSEADDR`
/// and the appropriate `IP_FREEBIND` option.
///
/// `socket_type` should be [`Type::STREAM`] for TCP or [`Type::DGRAM`] for UDP.
pub fn create_freebind_socket(addr: &SocketAddr, socket_type: Type) -> std::io::Result<Socket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };

    let socket = Socket::new(domain, socket_type, None)?;

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

// ---------------------------------------------------------------------------
// ReservedSocket — protocol-aware port reservation holder
// ---------------------------------------------------------------------------

/// A bound socket held as a port reservation.
///
/// When dropped, the underlying file descriptor is closed and the kernel
/// releases the port.
#[allow(dead_code)]
enum ReservedSocket {
    Tcp(TcpListener),
    Udp(UdpSocket),
}

// ---------------------------------------------------------------------------
// PortAllocator — runtime TCP/UDP pre-bind reservation
// ---------------------------------------------------------------------------

/// A concurrency-safe port reservation system backed by protocol-aware
/// socket pre-bind.
///
/// Ports are keyed by [`SocketAddr`]. Allocating a port binds either a
/// [`TcpListener`] (TCP) or [`UdpSocket`] (UDP) to it, preventing other
/// processes or concurrent daemon operations from claiming the same port.
pub struct PortAllocator {
    sockets: RwLock<HashMap<SocketAddr, ReservedSocket>>,
}

impl PortAllocator {
    pub fn new() -> Self {
        Self {
            sockets: RwLock::new(HashMap::new()),
        }
    }

    /// Reserves `addr` by binding a socket of the appropriate protocol.
    ///
    /// For TCP: binds and listens. For UDP: binds only (connectionless).
    ///
    /// # Errors
    ///
    /// Returns an error if the port is already bound by another process.
    pub async fn allocate(&self, addr: SocketAddr, proto: TransportProtocol) -> Result<()> {
        // `deallocate` releases by dropping the socket, and the port is not
        // always bindable again on the very next attempt — the fd outlives the
        // map entry by a moment. Releasing a stale mapping and immediately
        // re-allocating the same port is a real daemon path (a recreated
        // container keeps its host port), so retry briefly rather than fail.
        const ATTEMPTS: u32 = 25;
        const BACKOFF: std::time::Duration = std::time::Duration::from_millis(2);
        let mut last = None;
        for attempt in 0..ATTEMPTS {
            match self.allocate_once(addr, proto).await {
                Ok(()) => return Ok(()),
                Err(e) => {
                    last = Some(e);
                    if attempt + 1 < ATTEMPTS {
                        tokio::time::sleep(BACKOFF).await;
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| eyre!("Failed to reserve {addr}")))
    }

    async fn allocate_once(&self, addr: SocketAddr, proto: TransportProtocol) -> Result<()> {
        let socket_type = match proto {
            TransportProtocol::Tcp => Type::STREAM,
            TransportProtocol::Udp => Type::DGRAM,
        };

        let socket = create_freebind_socket(&addr, socket_type)
            .map_err(|e| eyre!("Failed to create socket for {addr}: {e}"))?;

        socket
            .bind(&addr.into())
            .map_err(|e| eyre!("Failed to reserve {addr}: {e}"))?;

        match proto {
            TransportProtocol::Tcp => {
                socket
                    .listen(128)
                    .map_err(|e| eyre!("Failed to listen on {addr}: {e}"))?;

                let std_listener: std::net::TcpListener = socket.into();
                std_listener
                    .set_nonblocking(true)
                    .map_err(|e| eyre!("Failed to set nonblocking for {addr}: {e}"))?;

                let listener = TcpListener::from_std(std_listener)
                    .map_err(|e| eyre!("Failed to create tokio listener for {addr}: {e}"))?;

                info!("Reserved {addr} (TCP)");
                self.sockets
                    .write()
                    .await
                    .insert(addr, ReservedSocket::Tcp(listener));
            }
            TransportProtocol::Udp => {
                // UDP is connectionless — no listen() needed.
                let std_socket: std::net::UdpSocket = socket.into();
                std_socket
                    .set_nonblocking(true)
                    .map_err(|e| eyre!("Failed to set nonblocking for {addr}: {e}"))?;

                let udp_socket = UdpSocket::from_std(std_socket)
                    .map_err(|e| eyre!("Failed to create tokio UdpSocket for {addr}: {e}"))?;

                info!("Reserved {addr} (UDP)");
                self.sockets
                    .write()
                    .await
                    .insert(addr, ReservedSocket::Udp(udp_socket));
            }
        }

        Ok(())
    }

    /// Releases the reservation for `addr`, if any.
    pub async fn deallocate(&self, addr: SocketAddr) {
        self.sockets.write().await.remove(&addr);
        info!("Released {addr}");
    }

    /// Returns `true` if `addr` has an active reservation.
    pub async fn is_allocated(&self, addr: SocketAddr) -> bool {
        self.sockets.read().await.contains_key(&addr)
    }

    /// Releases all active reservations.
    pub async fn deallocate_all(&self) {
        let mut sockets = self.sockets.write().await;
        let count = sockets.len();
        sockets.clear();
        info!("Released all {count} reservations");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[tokio::test]
    async fn released_port_is_immediately_reallocatable() {
        // Load check, not a reproduction. The rebind this covers failed
        // intermittently only in the full parallel `lab-ops_natmap --lib`
        // suite (5 failures in 40 runs) and never in this binary, so it cannot
        // be turned into a test that fails without the retry in `allocate`.
        // What this does guard: 2560 rebinds under concurrency, plus — in
        // `allocate_fails_for_port_held_outside_the_allocator` — that the
        // retry does not paper over a genuine conflict.
        let allocator = Arc::new(PortAllocator::new());
        let mut tasks = Vec::new();
        for t in 0..64u16 {
            let allocator = allocator.clone();
            tasks.push(tokio::spawn(async move {
                for i in 0..40u16 {
                    let addr: SocketAddr =
                        format!("127.0.0.1:{}", 21000 + t * 40 + i).parse().unwrap();
                    allocator
                        .allocate(addr, TransportProtocol::Tcp)
                        .await
                        .unwrap();
                    allocator.deallocate(addr).await;
                    allocator
                        .allocate(addr, TransportProtocol::Tcp)
                        .await
                        .unwrap_or_else(|e| panic!("rebind of {addr} failed: {e:#}"));
                }
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }
    }

    #[tokio::test]
    async fn allocate_fails_for_port_held_outside_the_allocator() {
        // The retry must not turn a genuine conflict into a success.
        let allocator = PortAllocator::new();
        let addr: SocketAddr = "127.0.0.1:21999".parse().unwrap();
        let held = std::net::TcpListener::bind(addr).unwrap();
        assert!(
            allocator
                .allocate(addr, TransportProtocol::Tcp)
                .await
                .is_err()
        );
        drop(held);
    }

    #[tokio::test]
    async fn is_allocated_returns_true_for_reserved_port() {
        let allocator = PortAllocator::new();
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        allocator
            .allocate(addr, TransportProtocol::Tcp)
            .await
            .unwrap();
        assert!(allocator.is_allocated(addr).await);
    }

    #[tokio::test]
    async fn is_allocated_returns_false_after_release() {
        let allocator = PortAllocator::new();
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        allocator
            .allocate(addr, TransportProtocol::Tcp)
            .await
            .unwrap();
        allocator.deallocate(addr).await;
        assert!(!allocator.is_allocated(addr).await);
    }

    #[tokio::test]
    async fn is_allocated_returns_false_for_unreserved_port() {
        let allocator = PortAllocator::new();
        let addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        assert!(!allocator.is_allocated(addr).await);
    }

    #[tokio::test]
    async fn allocate_udp_binds_dgram() {
        let allocator = PortAllocator::new();
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        allocator
            .allocate(addr, TransportProtocol::Udp)
            .await
            .unwrap();
        assert!(allocator.is_allocated(addr).await);
        allocator.deallocate(addr).await;
        assert!(!allocator.is_allocated(addr).await);
    }
}
