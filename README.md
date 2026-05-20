# lab-ops

Personal utility tools for my homelab.

## Usage

```bash
lab-ops <cmd> [args...]
```

### dockernet

Prints IP addresses and port bindings of Docker containers

```
lab-ops dockernet
```

### natmap

```bash
lab-ops natmap <command> [args...]
```

Manage iptables NAT rules for static VMs and dynamic Docker port remapping.

#### Static VM NAT Rules

Add or delete DNAT forwarding rules:
```bash
lab-ops natmap dnat --ext-ip 203.0.113.43 --int-ip 10.0.0.101 --ports 25,465
lab-ops natmap dnat --ext-ip 203.0.113.43 --int-ip 10.0.0.101 --ports 25,465 --delete
```

Add or delete SNAT rules:
```bash
lab-ops natmap snat --ext-ip 203.0.113.43 --int-ip 10.0.0.101 --ext-if vmbr0
```

Add or delete hairpin NAT rules:
```bash
lab-ops natmap hairpin --ext-ip 203.0.113.43 --int-ip 10.0.0.101 --ports 25,465
```

Enable IP forwarding and persist rules:
```bash
lab-ops natmap fwd
lab-ops natmap save
```

#### Dynamic Docker Port Remapping

The `natmap` daemon installs iptables DNAT rules in the `NATMAP` chain and exposes an API to remap host ports at runtime without restarting containers.

**Daemon**

```bash
# Run the daemon
sudo lab-ops natmap daemon

# Run with custom paths (for testing)
lab-ops natmap daemon --state-dir /tmp/natmap --socket /tmp/natmap.sock
```

**Install as systemd service**

```bash
sudo lab-ops natmap install
```

Creates a `natmap` group, adds the current user to it, copies the binary to `/usr/local/bin/lab-ops`, writes a systemd service file, and enables + starts it. Users in the `natmap` group can use the CLI without sudo (re-login required after install).

**Manage Mappings**

```bash
# List all NAT rules (static iptables + Docker)
lab-ops natmap ls
lab-ops natmap ls <container-id-or-name>

# Add a new mapping
lab-ops natmap docker add my-nginx 8080:80
lab-ops natmap docker add my-nginx 100.64.0.10:80:80
lab-ops natmap docker add my-nginx 8443:443/tcp

# Remap a host port
lab-ops natmap docker remap my-nginx 8080:9090

# Remove a mapping
lab-ops natmap docker rm my-nginx 8080/tcp
lab-ops natmap docker rm --id 1
```

### auto-discover

`crates/auto-discover/` — Service discovery daemon that watches Docker events, manages port forwarding via `lab-ops natmap`, registers services with Consul, and generates nginx configs stored in Consul KV. See [docs/auto-discover/usage.md](docs/auto-discover/usage.md) for full documentation.

```bash
lab-ops auto-discover daemon                                 # Run unified daemon
lab-ops auto-discover daemon --no-forwarding --no-nginx      # Discovery only
lab-ops auto-discover daemon --no-discovery                  # Forwarding + nginx only
lab-ops auto-discover sync                                   # Single-sync pass
lab-ops auto-discover check                                  # Validate config
lab-ops auto-discover forwarding-sync [--consul-addr URL]    # One-shot DNAT sync
```

## License

[MIT](https://spdx.org/licenses/MIT.html)
