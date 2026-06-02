## Terms

- **natmap daemon**: Central authority for ALL iptables NAT rules. Installs DNAT rules in the `NATMAP` chain, manages port reservations, persists state to `/var/lib/natmap/state.json`
- **natmap CLI**: Communicates with the daemon via Unix socket (`/run/natmap.sock`) for all rule management commands
- **DNAT**: Destination NAT — forwards traffic from an external IP:port to an internal host. Creates PREROUTING and FORWARD rules
- **SNAT**: Source NAT — rewrites source IP of outgoing traffic from internal hosts. Creates POSTROUTING rules
- **Hairpin NAT**: Allows internal hosts to reach themselves via the external IP. Creates PREROUTING DNAT + POSTROUTING MASQUERADE rules
- **Docker mapping**: Dynamic port remapping managed by natmap — maps a host port to a container port without restarting the container

## Daemon

The natmap daemon is the central authority for all iptables NAT rules. All rule management commands (`dnat`, `snat`, `hairpin`, `docker add`, `docker rm`, `docker remap`) require the daemon to be running. The daemon handles port reservation, state persistence, and iptables rule management.

### Starting

```bash
# Install as a systemd service (recommended)
sudo lab-ops natmap install

# Or run manually
sudo lab-ops natmap daemon

# Run with custom paths (for testing)
lab-ops natmap daemon \
  --state /tmp/natmap_state.json \
  --socket /tmp/natmap.sock \
  --socket-group root
```

### Systemd Installation

```bash
sudo lab-ops natmap install
```

This command:
1. Copies the `lab-ops` binary to `/usr/local/bin/`
2. Creates a `natmap` system group and adds your user to it
3. Writes `/etc/systemd/system/natmap.service`
4. Runs `systemctl daemon-reload` and `systemctl enable --now natmap`

After re-login (for group membership), you can use `lab-ops natmap ...` without `sudo`.

### Service Control

```bash
# Check status
systemctl status natmap

# View logs
journalctl -u natmap -f

# Restart after binary update
sudo systemctl restart natmap
```

### State Persistence

The daemon persists state to `/var/lib/natmap/state.json`. On restart or crash recovery, it:
- Flushes all stale iptables rules
- Releases all port reservations
- Reads the state file and rebinds only rules whose ports are still available
- Skips rules for ports taken by other services (logged as warnings)

### Socket

All CLI commands communicate with the daemon via a Unix socket. Default: `/run/natmap.sock`.

```bash
# Non-default socket
lab-ops natmap --socket /tmp/natmap.sock dnat --ext-ip ... --int-ip ... --ports 80
lab-ops natmap --socket /tmp/natmap.sock ls
```

## CLI Commands

### Global Options

| Option | Default | Description |
|--------|---------|-------------|
| `--socket` | `/run/natmap.sock` | Path to the natmap daemon Unix socket |
| `--json` | off | Output in JSON format instead of tables |
| `--color` | `auto` | Output coloring: `auto`, `always`, `never`. Table headers are colorized when enabled |

Verbosity is controlled via the root-level `-v` / `--verbose` flag (repeatable: `-v`, `-vv`, `-vvv+`).

### Utility Commands

```bash
# Enable IP forwarding (required for DNAT to work)
lab-ops natmap fwd

# Save current iptables rules to disk
lab-ops natmap save

# Clear all managed NAT rules and reset daemon state
lab-ops natmap clear
```

The `clear` command removes all daemon-managed rules (static DNAT, SNAT, hairpin, and Docker port mappings), releases all port reservations, and resets the persisted state. It is useful for bulk cleanup without restarting the daemon.

### Policy Routing

Manage Linux policy routing rules for source IP preservation. When `preserve_src_ip: true` is configured in auto-discover, the service node needs a policy route so return traffic routes back through the proxy gateway instead of directly to the client. This preserves the real sender IP end-to-end.

The daemon also clones non-default, non-local routes from the main routing table into the policy table (e.g., Docker bridge subnets) so local traffic uses the correct interface instead of the proxy gateway.

```bash
# Add a policy route: packets from SRC_IP use table TABLE with default via GATEWAY
lab-ops natmap policy-route --src-ip 10.0.0.101 --via 10.0.0.1 --table 100

# Remove the policy route
lab-ops natmap policy-route --src-ip 10.0.0.101 --via 10.0.0.1 --table 100 --delete
```

The `policy-route` command talks to the natmap daemon via Unix socket. It requires `CAP_NET_ADMIN` (privileged container or root).

### Static NAT Rules

#### DNAT (Destination NAT)

Forward traffic from an external IP/port to an internal host. Creates PREROUTING and FORWARD rules.

```bash
# Forward ports 25 and 465
lab-ops natmap dnat \
  --ext-ip 203.0.113.43 \
  --int-ip 10.0.0.101 \
  --ports 25,465

# Forward a single port with a specific protocol
lab-ops natmap dnat \
  --ext-ip 100.64.0.1 \
  --int-ip 192.168.1.50 \
  --ports 8443 \
  --proto tcp

# Restrict to a specific interface
lab-ops natmap dnat \
  --ext-ip 203.0.113.43 \
  --int-ip 10.0.0.101 \
  --ports 80,443 \
  --ext-if vmbr0

# Delete rules
lab-ops natmap dnat \
  --ext-ip 203.0.113.43 \
  --int-ip 10.0.0.101 \
  --ports 25,465 \
  --delete
```

**Port reservation:** The daemon binds each port to `0.0.0.0` before creating iptables rules, preventing other services from claiming the port.

#### SNAT (Source NAT)

Rewrite the source IP of outgoing traffic from an internal host. Creates POSTROUTING rules.

```bash
# Add SNAT rule
lab-ops natmap snat \
  --int-ip 10.0.0.101 \
  --ext-if vmbr0 \
  --ext-ip 203.0.113.43

# Delete SNAT rule (same flags + --delete)
lab-ops natmap snat \
  --int-ip 10.0.0.101 \
  --ext-if vmbr0 \
  --ext-ip 203.0.113.43 \
  --delete
```

SNAT does **not** reserve a port since it only modifies source addresses for outgoing traffic.

#### Hairpin NAT

Allows internal hosts to reach a service via the external IP. Creates POSTROUTING MASQUERADE rules, and optionally a PREROUTING DNAT rule for self-connections.

**Full hairpin** (default): Creates a PREROUTING DNAT rule for the internal host's self-connection and a POSTROUTING MASQUERADE matching all sources (`-s 0.0.0.0/0`). Use when `preserve_src_ip` is `false`:

```bash
# Add full hairpin NAT
lab-ops natmap hairpin \
  --ext-ip 203.0.113.43 \
  --int-ip 10.0.0.101 \
  --ports 25,465,587

# Delete hairpin NAT
lab-ops natmap hairpin \
  --ext-ip 203.0.113.43 \
  --int-ip 10.0.0.101 \
  --ports 25,465,587 \
  --delete
```

**LAN-limited hairpin** (`--lan-cidr`): Skips the PREROUTING DNAT (the regular DNAT rule handles forward traffic) and creates a POSTROUTING MASQUERADE matching only sources in the given LAN subnet. Use when `preserve_src_ip` is `true` — this enables LAN client hairpin without breaking source IP preservation for WAN clients:

```bash
# Add LAN-limited hairpin MASQUERADE
lab-ops natmap hairpin \
  --ext-ip 203.0.113.43 \
  --int-ip 10.0.0.101 \
  --ports 25565 \
  --lan-cidr 10.0.0.0/24

# Delete (no --lan-cidr needed, matched by comment)
lab-ops natmap hairpin \
  --ext-ip 203.0.113.43 \
  --int-ip 10.0.0.101 \
  --ports 25565 \
  --delete
```

The `--lan-cidr` flag produces the iptables rule:
```
-A POSTROUTING -s 10.0.0.0/24 -d 10.0.0.101 -p tcp --dport 25565 -j MASQUERADE
```

Without `--lan-cidr` (full hairpin), it produces:
```
-A PREROUTING -s 10.0.0.101 -d 203.0.113.43 -p tcp --dport 25565 -j DNAT --to-destination 10.0.0.101
-A POSTROUTING -s 0.0.0.0/0 -d 10.0.0.101 -p tcp --dport 25565 -j MASQUERADE
```

### Docker Container Mappings

All Docker commands require the daemon to have access to the Docker socket (`/var/run/docker.sock`). If Docker is unavailable, the daemon will start without Docker support and Docker commands will return an error.

#### Listing Mappings

```bash
# List all rules (static iptables + Docker mappings)
lab-ops natmap ls

# Filter by container ID or name
lab-ops natmap ls nginx
lab-ops natmap ls abc123def456
```

#### Adding a Mapping

```bash
# Map host port 8080 to container port 80 (all interfaces)
lab-ops natmap docker add my-nginx 8080:80

# Specify container name instead of ID (--name flag)
lab-ops natmap docker add 8080:80 --name my-nginx

# Bind to a specific host IP
lab-ops natmap docker add my-nginx 100.64.0.10:8080:80

# Specify protocol
lab-ops natmap docker add my-nginx 8443:443/tcp

# UDP mapping
lab-ops natmap docker add my-dns 53:53/udp

# Map to a specific target IP (skips Docker inspect)
lab-ops natmap docker add 8080:127.0.0.1:80 --name my-local-service

# Host port only — container port is the same
lab-ops natmap docker add my-nginx 8080
```

#### Removing a Mapping

```bash
# Remove by container + host port
lab-ops natmap docker rm my-nginx 8080

# Remove a local service mapping by name
lab-ops natmap docker rm --name my-local-service 8080

# Remove by mapping ID
lab-ops natmap docker rm --id 3
```

#### Remapping a Port

Change an existing mapping's host port without restarting the container.

```bash
# Change host port from 8080 to 9090
lab-ops natmap docker remap my-nginx 8080:9090
```

## JSON Output

Any `natmap` subcommand supports `--json` for machine-readable output:

```bash
lab-ops natmap --json ls
lab-ops natmap --json docker add my-nginx 8080:80
```

## Real-World Examples

### Mail Server Port Forwarding

Expose all mail-related ports from a public IP to an internal VM:

```bash
#!/bin/bash
INT_IP="10.0.0.101"
EXT_IP="203.0.113.43"
EXT_IF="vmbr0"
PORTS="25,465,587,143,993,110,995,4190"

# Enable forwarding
lab-ops natmap fwd

# Clear any previous rules (same effect as individual --delete)
lab-ops natmap clear || true

# Apply new rules
lab-ops natmap dnat --ext-ip $EXT_IP --int-ip $INT_IP --ports $PORTS
lab-ops natmap snat --int-ip $INT_IP --ext-if $EXT_IF --ext-ip $EXT_IP
lab-ops natmap hairpin --ext-ip $EXT_IP --int-ip $INT_IP --ports $PORTS

lab-ops natmap save
echo "Mail NAT applied: $EXT_IP -> $INT_IP"
```

### Web Server with Game Server on a Tailscale Exit Node

```bash
# Forward web traffic
lab-ops natmap dnat \
  --ext-ip 100.64.0.10 \
  --int-ip 10.0.1.20 \
  --ports 80,443

# Forward game server (single port, no interface restriction)
lab-ops natmap dnat \
  --ext-ip 100.64.0.10 \
  --int-ip 10.0.1.30 \
  --ports 25565 \
  --proto tcp

# SNAT for outbound traffic
lab-ops natmap snat \
  --int-ip 10.0.1.0/24 \
  --ext-if tailscale0 \
  --ext-ip 100.64.0.10
```

### Docker Container Port Management

```bash
# Expose container ports without restarting
lab-ops natmap docker add nginx-proxy 8443:443/tcp
lab-ops natmap docker add grafana 3000:3000

# List everything
lab-ops natmap ls

# Remove a mapping when no longer needed
lab-ops natmap docker rm grafana 3000
```
