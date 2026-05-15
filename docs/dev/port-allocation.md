# Port Allocation System

See [../diagrams/port-allocation.png](../diagrams/port-allocation.png) for the sequence diagram.

## Why Port Allocation?

Without port reservation, `natmap` only creates iptables rules to redirect traffic. This means:

- Another service could bind to the same port on the host, creating a conflict
- Docker's own `docker-proxy` would bind first, preventing natmap from taking over
- After a crash, the port could be claimed by a new service before the daemon restarts

## How It Works

The `PortAllocator` struct maintains a `HashMap<SocketAddr, TcpListener>` keyed by `SocketAddr`. When a rule is added that needs a port reservation (DNAT, hairpin, Docker mapping):

1. A raw TCP socket is created and configured with the `IP_FREEBIND` (Linux) socket option
2. The socket is bound to the exact `{ext_ip}:{port}` using `TcpListener::bind()`
3. The kernel reserves the port, preventing any other process from binding to it
4. The `TcpListener` is stored in the HashMap (keeping the reservation alive)
5. iptables rules are then installed (redirecting traffic away from the bound socket)

When a rule is removed:

1. iptables rules are deleted first
2. The `TcpListener` entry is removed from the HashMap
3. Rust drops the `TcpListener`, which closes the socket and releases the port

## Why IP_FREEBIND?

Instead of binding to `0.0.0.0` or failing when an IP isn't present, the socket uses Linux's `IP_FREEBIND` capability. This solves two major issues:

- **Floating IPs**: The external IP might not be configured on any local network interface (e.g., Tailscale IPs, HA floating IPs, or pending interfaces). `IP_FREEBIND` instructs the kernel to allow binding to these non-local IPs.
- **Port Sharing**: By binding to the exact external IP instead of `0.0.0.0`, `natmap` allows the same port to be reserved simultaneously under different external IPs (e.g., mapping both `1.1.1.1:80` and `2.2.2.2:80`). Binding to `0.0.0.0` would have prevented this.

## Port Reservation Keys

Keys are the actual `SocketAddr` object (e.g., `139.99.69.43:25`). The IP in the key is the external IP from the DNAT/hairpin config.

## Which Rules Reserve Ports?

| Rule Type | Reserves Port? | Reason |
|-----------|---------------|--------|
| DNAT | Yes | Directs incoming traffic to internal host |
| Hairpin | Yes | Allows internal host to reach itself via external IP |
| Docker add | Yes | Redirects host port to container |
| Docker remap | Yes (new port) | Changes existing mapping's host port |
| SNAT | No | Only modifies outbound source addresses |

## Error Handling

If `TcpListener::bind()` fails (e.g. port is already in use by another application on that IP):
- The daemon returns HTTP `409 Conflict` to the CLI
- No iptables rules are created
- No state changes are persisted

If `IptablesManager::install_*()` fails after successful port binding:
- All reserved ports for that request are released (`deallocate`)
- Returns HTTP `500 Internal Server Error`
- No state changes are persisted
