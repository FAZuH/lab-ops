//! Port checking and socket setup utilities.
//!
//! Provides utilities for robust port checking and freebinding.

use std::net::SocketAddr;
use std::net::ToSocketAddrs;

use socket2::Domain;
use socket2::Socket;
use socket2::Type;

/// Creates and configures a `Socket` for `addr` with `SO_REUSEADDR`
/// and the appropriate `IP_FREEBIND` option.
pub fn create_freebind_socket(addr: &SocketAddr) -> std::io::Result<Socket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };

    let socket = Socket::new(domain, Type::STREAM, None)?;

    // Without this we may fail to bind if the socket was just released and the port is briefly in TIME_WAIT.
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
