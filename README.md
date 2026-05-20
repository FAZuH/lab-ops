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

Manage iptables NAT rules for static VMs and dynamic Docker port remapping. Runs as a systemd daemon with a Unix socket API. See [docs/natmap/usage.md](docs/natmap/usage.md) for full documentation.

```bash
lab-ops natmap daemon                   # Start the daemon
sudo lab-ops natmap install             # Install as systemd service
lab-ops natmap dnat --ext-ip ... --int-ip ... --ports 80  # Static DNAT
lab-ops natmap docker add nginx 8080:80 # Docker port mapping
lab-ops natmap ls                       # List all rules
lab-ops natmap clear                    # Remove all managed rules
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
lab-ops auto-discover nginx-sync [--consul-addr URL]         # One-shot nginx config sync
```

## License

[MIT](https://spdx.org/licenses/MIT.html)
