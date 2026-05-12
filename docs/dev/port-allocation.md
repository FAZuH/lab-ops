# Port Allocation System

See [../diagrams/port-allocation.png](../diagrams/port-allocation.png) for the sequence diagram.

## Why Port Allocation?

Without port reservation, `natmap` only creates iptables rules to redirect traffic. This means:

- Another service could bind to the same port on the host, creating a conflict
- Docker's own `docker-proxy` would bind first, preventing natmap from taking over
- After a crash, the port could be claimed by a new service before the daemon restarts

## How It Works

The `PortAllocator` struct maintains a `HashMap<String, TcpListener>` keyed by `"{ip}:{port}"`. When a rule is added that needs a port reservation (DNAT, hairpin, Docker mapping):

1. `TcpListener::bind("0.0.0.0:{port}")` is called
2. The kernel reserves the port, preventing any other process from binding to it
3. The `TcpListener` is stored in the HashMap (keeping the reservation alive)
4. iptables rules are then installed (redirecting traffic away from the bound socket)

When a rule is removed:

1. iptables rules are deleted first
2. The `TcpListener` entry is removed from the HashMap
3. Rust drops the `TcpListener`, which closes the socket and releases the port

## Why 0.0.0.0?

The port is always bound to `0.0.0.0` regardless of the external IP in the DNAT rule. This is because:
- The external IP may not be configured on the local machine (it could be a floating IP or Tailscale IP)
- Binding to `0.0.0.0` reserves the port for ALL interfaces
- The iptables rules handle the IP-specific routing decisions

## Port Reservation Keys

Keys are formatted as `"{ip}:{port}"` (e.g., `"139.99.69.43:25"`). The IP in the key is the external IP from the DNAT/hairpin config, used for uniqueness. This allows the same port to be reserved under different external IPs (though binding is always to `0.0.0.0`).

## Which Rules Reserve Ports?

| Rule Type | Reserves Port? | Reason |
|-----------|---------------|--------|
| DNAT | Yes | Directs incoming traffic to internal host |
| Hairpin | Yes | Allows internal host to reach itself via external IP |
| Docker add | Yes | Redirects host port to container |
| Docker remap | Yes (new port) | Changes existing mapping's host port |
| SNAT | No | Only modifies outbound source addresses |

## Error Handling

If `TcpListener::bind()` fails:
- The daemon returns HTTP `409 Conflict` to the CLI
- No iptables rules are created
- No state changes are persisted

If `IptablesManager::install_*()` fails after successful port binding:
- All reserved ports are released (`deallocate`)
- Returns HTTP `500 Internal Server Error`
- No state changes are persisted
