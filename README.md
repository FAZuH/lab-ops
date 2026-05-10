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
lab-ops natmap forward --ext-ip 139.99.69.43 --int-ip 10.10.10.101 --ports 25,465
lab-ops natmap forward --ext-ip 139.99.69.43 --int-ip 10.10.10.101 --ports 25,465 --delete
```

Add or delete SNAT rules:
```bash
lab-ops natmap snat --ext-ip 139.99.69.43 --int-ip 10.10.10.101 --ext-if vmbr0
```

Add or delete hairpin NAT rules:
```bash
lab-ops natmap hairpin --ext-ip 139.99.69.43 --int-ip 10.10.10.101 --ports 25,465
```

Enable IP forwarding and persist rules:
```bash
lab-ops natmap enable-forwarding
lab-ops natmap persist
```

#### Dynamic Docker Port Remapping

The `natmap` daemon installs iptables DNAT rules in the `DOCKER-USER` chain and exposes an API to remap host ports at runtime without restarting containers.

**Daemon**

```bash
# Run the daemon
sudo lab-ops natmap daemon

# Run with custom paths (for testing)
lab-ops natmap daemon --state-dir /tmp/dockernatmap --socket /tmp/dockernatmap.sock
```

**Install as systemd service**

```bash
sudo lab-ops natmap install
```

Creates a `dockernatmap` group, adds the current user to it, copies the binary to `/usr/local/bin/lab-ops`, writes a systemd service file, and enables + starts it. Users in the `dockernatmap` group can use the CLI without sudo (re-login required after install).

**Manage Mappings**

```bash
# List mappings
lab-ops natmap list
lab-ops natmap list <container-id-or-name>

# Add a new mapping
lab-ops natmap add my-nginx 8080:80
lab-ops natmap add my-nginx 100.64.0.10:80:80
lab-ops natmap add my-nginx 8443:443/tcp

# Remap a host port
lab-ops natmap remap my-nginx 8080:9090

# Remove a mapping
lab-ops natmap remove my-nginx 8080/tcp
lab-ops natmap remove --id 1
```

## License

[MIT](https://spdx.org/licenses/MIT.html)
