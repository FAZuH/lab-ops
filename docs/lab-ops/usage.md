# lab-ops Usage Guide

## Installation

```bash
cargo build --release
sudo cp target/release/lab-ops /usr/local/bin/
```

## Overview

`lab-ops` is a homelab operations toolkit with three main commands:

| Command | Purpose |
|---------|---------|
| `dockernet` | List IP addresses and port bindings of Docker containers |
| `cf2ansible` | Convert BIND DNS zone files to Ansible Cloudflare DNS tasks |
| `natmap` | Manage iptables NAT rules (static VMs & dynamic Docker mappings) |

---

## Global Options

The `natmap` command accepts global options that apply to all subcommands:

| Option | Default | Description |
|--------|---------|-------------|
| `--socket` | `/run/natmap.sock` | Path to the natmap daemon Unix socket |
| `--json` | off | Output in JSON format instead of tables |

---

## dockernet

Lists Docker container network information.

```bash
lab-ops dockernet
```

Displays a table with container names, network names, IP addresses, and port bindings.

---

## cf2ansible

Converts BIND DNS zone files into Ansible tasks for Cloudflare DNS management.

```bash
lab-ops cf2ansible /path/to/zone-file.txt

# Override the zone name (defaults to SOA record)
lab-ops cf2ansible /path/to/zone-file.txt example.com
```

The output is YAML suitable for use with `community.general.cloudflare_dns`.

---

## natmap

```bash
lab-ops natmap <command> [args...]
```

Manage iptables NAT rules for static VMs and dynamic Docker port remapping. All rule management commands communicate with the natmap daemon via Unix socket. See [docs/natmap/usage.md](../natmap/usage.md) for full documentation.

Quick reference:
```bash
sudo lab-ops natmap daemon              # Start the daemon
sudo lab-ops natmap install             # Install as systemd service
lab-ops natmap dnat --ext-ip ... --int-ip ... --ports 80  # Static DNAT
lab-ops natmap docker add nginx 8080:80 # Docker port mapping
lab-ops natmap ls                       # List all rules
lab-ops natmap clear                    # Remove all managed rules
```
