# lab-ops

Personal utility tools for my homelab.

## Installation

You can build and install the tool using Cargo:

```bash
# Install latest versoin from crates.io
cargo install lab-ops

# Install directly from git
cargo install --git https://github.com/fazuh/lab-ops

# Or build directly from project root
cargo install --path .
```

Or download the pre-compiled binary at https://github.com/FAZuH/lab-ops/releases

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
```

## Global Options

| Flag | Description |
|------|-------------|
| `-v` / `-vv` / `-vvv+` | Increase verbosity (info → debug → trace) |
| `--color auto\|always\|never` | Control ANSI color output (default: auto, respects `NO_COLOR`) |

## Shell Completions

Generate completion scripts for bash, zsh, fish, powershell, or elvish:

```bash
# Write to a completions directory
lab-ops completions bash --dir ~/.local/share/bash-completion/completions
lab-ops completions zsh --dir ~/.config/zsh/completions       # add dir to $fpath
lab-ops completions fish --dir ~/.config/fish/completions

# Or append to your .rc file (bash / zsh)
echo 'eval "$(lab-ops completions bash)"' >> ~/.bashrc
echo 'eval "$(lab-ops completions zsh)"' >> ~/.zshrc
```

After installing, restart your shell or run `source ~/.zshrc` / `source ~/.bashrc` to activate.

## License

[MIT](https://spdx.org/licenses/MIT.html)
