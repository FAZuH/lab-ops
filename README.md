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

### dockernatmap

Dynamic Docker port remapping daemon. Installs iptables DNAT rules in the `DOCKER-USER` chain and exposes an API to remap host ports at runtime without restarting containers. Coexists with Docker's own iptables management.

#### Daemon

```bash
# Run the daemon
sudo lab-ops dockernatmap daemon

# Run with custom paths (for testing)
lab-ops dockernatmap daemon --state-dir /tmp/dockernatmap --socket /tmp/dockernatmap.sock
```

#### Install as systemd service

```bash
sudo lab-ops dockernatmap install
```

Creates a `dockernatmap` group, adds the current user to it, copies the binary to `/usr/local/bin/lab-ops`, writes a systemd service file, and enables + starts it. Users in the `dockernatmap` group can use the CLI without sudo (re-login required after install).

#### List mappings

```bash
lab-ops dockernatmap list

lab-ops dockernatmap list <container-id-or-name>
```

#### Add a new mapping

```bash
# Map host port 8080 to container port 80 (all interfaces)
lab-ops dockernatmap add my-nginx 8080:80

# Map a specific host IP
lab-ops dockernatmap add my-nginx 100.64.0.10:80:80

# Specify protocol
lab-ops dockernatmap add my-nginx 8443:443/tcp
```

#### Remap a host port

```bash
lab-ops dockernatmap remap my-nginx 8080:9090
```

#### Remove a mapping

```bash
# By container + port
lab-ops dockernatmap remove my-nginx 8080/tcp

# By mapping ID
lab-ops dockernatmap remove --id 1
```

#### JSON output

```bash
lab-ops dockernatmap --json list
```

#### Custom socket path

```bash
lab-ops dockernatmap --socket /tmp/dockernatmap.sock list
```

### cf2ansible

```bash
lab-ops cf2ansible <zone-file> [zone-name]
```

Converts a BIND DNS zone file into Ansible Cloudflare DNS tasks (community.general.cloudflare_dns).

```bash
lab-ops cf2ansible example.com.txt
lab-ops cf2ansible example.com.txt example.com
```

## License

[MIT](https://spdx.org/licenses/MIT.html)
