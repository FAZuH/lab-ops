## Terms

- **natmap daemon**: Central authority for ALL iptables NAT rules. Installs DNAT rules in the `NATMAP` chain, manages port reservations, persists state to `/var/lib/natmap/state.json`
- **natmap CLI**: Communicates with the daemon via Unix socket (`/run/natmap.sock`) for all rule management commands
- **DNAT**: Destination NAT — forwards traffic from an external IP:port to an internal host. Creates a PREROUTING DNAT plus an accept rule: `FORWARD` for a forwarded destination, `INPUT` for a locally delivered one (loopback)
- **SNAT**: Source NAT — rewrites source IP of outgoing traffic from internal hosts. Creates POSTROUTING rules
- **Hairpin NAT**: Allows internal hosts to reach themselves via the external IP. Creates PREROUTING DNAT + POSTROUTING MASQUERADE rules
- **Docker mapping**: Dynamic port remapping managed by natmap — maps a host port to a container port without restarting the container
- **Declaration**: A node's `discovery.yaml` — the host ports each service owns (`ports:`), and the address each answers on. natmap never allocates a port for a declared service: the declared port is the port used
- **Ownership ledger**: The `owner` the daemon records on every mapping an apply created, saying which service owns the port. A mapping with no owner was added by hand
- **Covered port**: A declared port whose service already answers on the node's tailnet address (or on every address), so it needs no mapping
- **Drift**: Anything the daemon holds that disagrees with the declaration. `plan` reports it and exits non-zero

## Declarative apply

`natmap plan` and `natmap apply --from <file>` converge one node to the mappings
its declaration names. The declaration is `nodes/<node>/discovery.yaml`:

```yaml
node:
  name: fazuh-sg-2
  tailnet_ip: 100.64.0.17    # how other nodes reach this one

defaults:
  bind_ip: 127.0.0.1         # where a declared port answers unless it says otherwise

services:
  fazuh-uptime:
    type: docker
    ports:                     # the ledger: ports this service owns
      - port: 3001
  fazuh-grafana:
    type: docker
    ports:
      - port: 3000
        bind_ip: 10.0.0.5      # this one answers on a concrete address
    nginx:                     # not natmap's business; ignored
      target: nodes/fazuh-sg-2/fazuh-grafana/nginx.conf
      context: http
```

One declared port produces one mapping: the node's `tailnet_ip` on that port,
forwarded to the address the port names (`bind_ip`, else `defaults.bind_ip`,
else `127.0.0.1`). The port is the same on both sides — nothing is allocated.

A port whose address is the tailnet IP, or a wildcard (`0.0.0.0` / `::`), is
already reachable there and is reported as **covered** instead of mapped. That
is the migration path: a container that still publishes on the tailnet address
needs no mapping, and one moved to loopback gets its mapping on the next apply.

### `plan`

```bash
lab-ops natmap plan --from nodes/fazuh-sg-2/discovery.yaml
lab-ops natmap plan --from nodes/fazuh-sg-2/discovery.yaml --prune
lab-ops natmap --json plan --from nodes/fazuh-sg-2/discovery.yaml
```

Prints what would change — `create`, `update`, `release`, `stale`, `keep`,
`covered`, `conflict` — writes nothing, and exits non-zero when the node does
not match the declaration. `--prune` changes what counts as drift: without it a
mapping this node created that the declaration dropped is reported `stale` and
kept.

### `apply`

```bash
lab-ops natmap apply --from nodes/fazuh-sg-2/discovery.yaml            # converge
lab-ops natmap apply --from nodes/fazuh-sg-2/discovery.yaml --prune    # also release
lab-ops natmap apply --from nodes/fazuh-sg-2/discovery.yaml --dry-run  # plan, write nothing
```

`--dry-run` prints the plan, writes nothing, and exits non-zero on drift —
identical to `plan`.

Order is part of the contract: creates and updates first, then releases. A port
this apply hands to another service is the one exception — that release runs
first, because the daemon arbitrates the port and would answer 409 to the
create.

### Ownership

The daemon records which service owns each port on the mapping itself, so the
mapping set is the ledger:

- A port is only ever handed to the service the declaration gives it to. A port
  owned by another service is refused, naming both.
- A port is released only by the apply that removes its owner. `--prune` is that
  apply; a plain `apply` refuses a port transfer and says so.
- Only a mapping an apply created is pruned. A mapping added by hand
  (`natmap docker add`) has no owner and survives every prune. If a hand-added
  mapping sits on a declared port, apply refuses rather than adopt it — remove
  it, then apply.
- A port claimed by two services in one declaration is a declaration error,
  naming both.

`apply` exits zero once it converged everything it was allowed to touch.

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

A missing state file starts with empty state. A present but unparseable state file (including a truncated write) is fatal: the daemon refuses to start with an error naming the path and the parse failure, and leaves the file untouched for inspection.

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

Manage Linux policy routing rules for source IP preservation. When a discovery service sets `preserve_src_ip: true`, the service node needs a policy route so return traffic routes back through the proxy gateway instead of directly to the client. This preserves the real sender IP end-to-end.

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

Forward traffic from an external IP/port to an internal host. Creates a PREROUTING DNAT and an accept rule in `FORWARD`, or in `INPUT` when the internal address is on this host (`127.0.0.1`, `::1`).

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

**Port reservation:** The daemon binds each port on the external address before creating iptables rules, preventing other services from claiming it. A loopback destination is the exception: the kernel rule alone services it, so no userspace listener is opened (a listener would accept locally delivered traffic itself instead of forwarding it). Docker and local-service mappings keep their reservation — the daemon allocates and arbitrates that host port through it.

**Forwarding to a host service on loopback:** `--int-ip 127.0.0.1` (or `::1`) forwards to a service listening on this host. The kernel must also allow the delivery: `net.ipv4.conf.<ingress>.route_localnet=1` for IPv4 (NixOS leaves it `0`), and an IPv6 destination is installed with `ip6tables`. The DNAT rule's counters increment either way, so a silent drop is a `route_localnet` problem, not a rule problem.

```bash
# tailnet 9100 -> node-exporter on 127.0.0.1:9100
lab-ops natmap dnat --ext-ip 100.64.0.10 --int-ip 127.0.0.1 --ports 9100
```

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

All Docker commands require the daemon to have access to the Docker socket (`/var/run/docker.sock`). If Docker is unavailable, the daemon starts without Docker support and Docker commands that need inspection return an error. This is not permanent: the daemon reinstalls persisted mappings from `state.json` as soon as it starts, keeps retrying the Docker connection with capped backoff (1 s doubling to a 30 s cap), and reconciles plus starts listening for container events once the Docker API answers — no restart needed when dockerd comes up after natmap.

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

Re-adding an existing mapping (same container, container port, protocol, and host IP) is idempotent: the daemon returns the live mapping with its host port unchanged and installs no duplicate rule. If the container's address changed, the rule is rewritten in place. This keeps repeated `apply` passes on a stable host port. Requesting a *different* explicit host port creates a second mapping for the same container port.

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
