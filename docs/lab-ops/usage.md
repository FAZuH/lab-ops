# lab-ops Usage Guide

## Installation

```bash
cargo build --release
sudo cp target/release/lab-ops /usr/local/bin/
```

## Overview

`lab-ops` is a homelab operations toolkit with these commands:

| Command | Purpose |
|---------|---------|
| `dockernet` | List IP addresses and port bindings of Docker containers |
| `cf2ansible` | Convert BIND DNS zone files to Ansible Cloudflare DNS tasks |
| `natmap` | Manage iptables NAT rules (static VMs & dynamic Docker mappings) |
| `auto-discover` | Service discovery daemon with Consul integration |
| `completions` | Generate shell completion scripts |

---

## Global Options

All commands accept these global options:

| Option | Default | Description |
|--------|---------|-------------|
| `-v` / `--verbose` | — | Repeat for higher verbosity: `-v` (info), `-vv` (debug), `-vvv+` (trace) |
| `--color` | `auto` | Output coloring: `auto` (terminal-detect), `always`, `never`. Also respects `NO_COLOR` / `CLICOLOR` env vars |

The `natmap` command also accepts these global options:

| Option | Default | Description |
|--------|---------|-------------|
| `--socket` | `/run/natmap.sock` | Path to the natmap daemon Unix socket |
| `--json` | off | Output in JSON format instead of tables |

---

## Shell Completions

Generate and install completion scripts (these scripts use dynamic evaluation to support runtime completions like Docker container names/IDs):

```bash
# Print to stdout (eval with quotes for zsh)
eval "$(lab-ops completions bash)"
eval "$(lab-ops completions zsh)"
source <(lab-ops completions fish)

# Write to a directory
lab-ops completions bash --dir ~/.local/share/bash-completion/completions
lab-ops completions zsh --dir ~/.config/zsh/completions
lab-ops completions fish --dir ~/.config/fish/completions
```

Shell values: `bash`, `elvish`, `fish`, `powershell`, `zsh`.

---

## dockernet

Lists Docker container network information.

```bash
lab-ops dockernet
```

Displays a table with container names, network names, IP addresses, and port bindings. Table headers are colorized when ANSI is enabled.

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
