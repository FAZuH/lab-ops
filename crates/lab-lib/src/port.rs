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

// --- Low-level socket utilities ---

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

// --- ReservedSocket ---

/// A bound socket held as a port reservation.
///
/// When dropped, the underlying file descriptor is closed and the kernel
/// releases the port.
#[allow(dead_code)]
enum ReservedSocket {
    Tcp(TcpListener),
    Udp(UdpSocket),
}

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
    /// Creates an allocator holding no reservations.
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
        // A released port is not always immediately bindable: the fd outlives
        // the map entry by a moment. A recreated container keeps its host port,
        // so retry briefly rather than fail the whole allocation.
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
    use std::net::IpAddr;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU16;
    use std::sync::atomic::Ordering;

    use super::*;

    /// Hands out `n` distinct host ports for a test to bind.
    ///
    /// A reservation is a real bind, so a hardcoded port collides with whatever
    /// else holds it — a concurrent run of this binary, a service on the box, or
    /// another test binary's band. Counted up from a per-process band, so each
    /// test's ports are distinct by construction and two concurrent runs of this
    /// binary land in different bands.
    ///
    /// The band starts at 12000, below the daemon's own `allocate_free_port`
    /// scan (32768..=61000) and below `lab-ops_natmap`'s test band (21000), so
    /// neither can take a port handed out here.
    fn test_ports(n: usize) -> Vec<u16> {
        static NEXT: AtomicU16 = AtomicU16::new(0);
        let base = 12000 + (std::process::id() as u16 % 400) * 8;
        (0..n)
            .map(|_| base + NEXT.fetch_add(1, Ordering::Relaxed))
            .collect()
    }

    fn loopback(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::from([127, 0, 0, 1]), port)
    }

    #[tokio::test]
    async fn released_port_is_immediately_reallocatable() {
        // Load check, not a reproduction: the rebind it covers failed only in
        // the full parallel `lab-ops_natmap --lib` suite, never in this binary.
        // It guards 2560 concurrent rebinds, and — with the test below — that
        // the retry does not paper over a genuine conflict.
        let ports = test_ports(64 * 40);
        let allocator = Arc::new(PortAllocator::new());
        let mut tasks = Vec::new();
        for chunk in ports.chunks(40) {
            let allocator = allocator.clone();
            let chunk = chunk.to_vec();
            tasks.push(tokio::spawn(async move {
                for port in chunk {
                    let addr = loopback(port);
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
        let addr = loopback(test_ports(1)[0]);
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
        let addr = loopback(test_ports(1)[0]);
        allocator
            .allocate(addr, TransportProtocol::Tcp)
            .await
            .unwrap();
        assert!(allocator.is_allocated(addr).await);
    }

    #[tokio::test]
    async fn is_allocated_returns_false_after_release() {
        let allocator = PortAllocator::new();
        let addr = loopback(test_ports(1)[0]);
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
        let addr = loopback(test_ports(1)[0]);
        assert!(!allocator.is_allocated(addr).await);
    }

    #[tokio::test]
    async fn allocate_udp_binds_dgram() {
        let allocator = PortAllocator::new();
        let addr = loopback(test_ports(1)[0]);
        allocator
            .allocate(addr, TransportProtocol::Udp)
            .await
            .unwrap();
        assert!(allocator.is_allocated(addr).await);
        allocator.deallocate(addr).await;
        assert!(!allocator.is_allocated(addr).await);
    }
}
