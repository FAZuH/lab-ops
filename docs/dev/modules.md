# Module Reference

## `crates/natmap/src/`

### `lib.rs` — Library Root

Declares all modules with `pub` visibility:

```rust
pub mod cli;
pub mod api;
pub mod command;
pub mod consts;
pub mod daemon;
pub mod docker;
pub mod install;
pub mod iptables;
pub mod models;
pub mod port_allocator;
pub mod utils;
```

### `cli.rs` — CLI Definitions

Defines the `NatMapCommand` enum with clap derives. Each variant maps to a subcommand:

```rust
pub enum NatMapCommand {
    Dnat { ext_ip, int_ip, proto, ports, ext_if, delete },
    Snat { int_ip, ext_if, ext_ip, delete },
    Hairpin { ext_ip, int_ip, proto, ports, delete },
    List { container_id },
    Docker { cmd: DockerCommand },
    Save,
    Fwd,
    Daemon { state_dir, socket, socket_group },
    Install { group, binary },
}

pub enum DockerCommand {
    Add { container_id, mapping },
    Remove { container_id, port, all, id },
    Remap { container_id, mapping },
}
```

Also defines the top-level `Cli` struct with global `--socket` and `--json` flags. The `run_cli_with_args()` function dispatches each variant to the appropriate handler in `command.rs`.

### `command.rs` — Handler Functions

Each subcommand has a handler function. Handlers for rule management (`handle_dnat`, `handle_snat`, `handle_hairpin`) serialize arguments to JSON and send HTTP requests to the daemon. Docker handlers (`add`, `remove`, `remap`) do the same against `/mapping/*` endpoints.

The `handle_list()` function combines raw `iptables-save` output with daemon-managed state for a complete view.

### `daemon.rs` — API Server & State

The largest module. Contains:

- **`AppState`**: Shared state (DaemonState, IptablesManager, PortAllocator, Docker client, next_id counter)
- **`run_daemon_with_paths()`**: Startup sequence (setup, reload, event listeners, graceful shutdown)
- **`reload_state()`**: Crash recovery — flushes stale rules, rebinds from state.json
- **`persist_state()`**: Atomic write of DaemonState to state.json (via temp file + rename)
- **`listen_docker_events()`**: Docker event stream handler for container start/die events
- **API handlers**: `add_dnat`, `remove_dnat`, `add_snat`, `remove_snat`, `add_hairpin`, `remove_hairpin`, `add_mapping`, `remove_mapping`, `remove_mapping_by_id`, `remap_port`, `list_mappings`

### `models.rs` — Data Types

Key types:

| Type | Purpose |
|------|---------|
| `DnatConfig` | Persisted DNAT rule (ext_ip, int_ip, ports, proto, ext_if) |
| `SnatConfig` | Persisted SNAT rule (int_ip, ext_ip, ext_if) |
| `HairpinConfig` | Persisted hairpin rule (ext_ip, int_ip, ports, proto) |
| `DnatRequest` / `SnatRequest` / `HairpinRequest` | API request bodies |
| `DaemonState` | Top-level persisted state (docker, dnats, snats, hairpins) |
| `ListResponse` | API response for `GET /mappings` |
| `ActivePortMapping` | Running Docker mapping (id, request, container info, comment) |
| `PortMappingRequest` | Docker mapping config (host_addr, container_addr, proto) |
| `AddMappingRequest` / `RemapRequest` | Docker API request bodies |

### `iptables.rs` — IptablesManager

Stateless manager for iptables operations. Key methods:

| Method | Purpose |
|--------|---------|
| `setup()` | Create NATMAP chains and jump rules in filter/nat tables |
| `flush_all_natmap()` | Flush ALL rules in NATMAP chains (crash recovery) |
| `install_mapping()` | Install Docker port mapping (DNAT + FORWARD + MASQUERADE + OUTPUT) |
| `remove_mapping()` | Remove Docker mapping by comment |
| `install_dnat()` / `remove_dnat()` | Static DNAT rules |
| `install_snat()` / `remove_snat()` | Static SNAT rules |
| `install_hairpin()` / `remove_hairpin()` | Static hairpin rules |
| `flush_container_rules()` | Remove all rules for a container |

### `port_allocator.rs` — PortAllocator

Manages socket-based port reservation:

```rust
pub struct PortAllocator {
    sockets: RwLock<HashMap<String, TcpListener>>,
}
```

| Method | Purpose |
|--------|---------|
| `allocate(key, addr)` | Bind `0.0.0.0:port` via TcpListener, store by key |
| `deallocate(key)` | Remove from map, drop Listener (releases port) |
| `is_allocated(key)` | Check if a key has an active reservation |
| `deallocate_all()` | Clear all reservations |

Port keys follow the format `"{ip}:{port}"` (e.g., `"203.0.113.43:8080"`).

### `docker.rs` — Docker Client

Wraps the `bollard` Docker API crate:
- `connect()` — Creates a bollard Docker client (reads `DOCKER_HOST` env var)
- `get_port_mappings()` — Inspects a container and extracts its port bindings

### `install.rs` — Systemd Installer

`install_systemd()` function that:
1. Copies the binary to the target path
2. Creates a `natmap` system group
3. Renders the systemd service template (substitutes `{binary}`, `{state_dir}`, `{group}`)
4. Writes `/etc/systemd/system/natmap.service`
5. Runs `systemctl daemon-reload` and `systemctl enable --now natmap`

### `utils.rs` — HTTP Client

Generic HTTP client for daemon communication:
- `request_json<T, R>(socket_path, method, path, body)` — Sends HTTP request to Unix socket, deserializes JSON response

## `crates/auto-discover/src/`

### `lib.rs` — Library Root

Declares all modules. Only `cli` is `pub` (consumed by the root `lab-ops` binary for CLI dispatch):

```rust
pub mod cli;
mod config;
mod consul;
mod daemon;
mod docker;
mod forwarding;
mod natmap;
mod nginx_daemon;
mod ports;
```

### `cli.rs` — CLI Definitions

Defines `Cli` struct and `Commands` enum with clap derives. The `run_cli()` function dispatches each variant. Subcommands:

| Variant | Purpose |
|---|---|
| `Daemon` | Unified long-running daemon (discovery + forwarding + nginx) |
| `Sync` | One-shot discovery sync pass |
| `Check` | Validate `discovery.yaml` |
| `ForwardingSync` | One-shot proxy-side DNAT rule sync |
| `NginxSync` | One-shot proxy-side nginx config sync |

### `config.rs` — DiscoveryConfig

Parses `/etc/auto-discover/discovery.yaml`:

- **`DiscoveryConfig`**: Top-level config (`name`, `bind_ip`, `bind_interface`, `networks`, `defaults`)
- **`NetworkEntry`**: Per-network config (`name`, `container_port`, `protocol`, `template`, `nginx_generator`, `forwarding`, `preprocess`, `postprocess`)
- **`ResolvedService`**: Fully resolved config with all defaults applied

### `consul.rs` — ConsulClient

Interacts with Consul Agent API:

- `register_service()` — Register/update a service with metadata
- `deregister_by_container()` — Remove all services matching a container ID
- `get_services_by_container()` — Query services by container ID
- `get_forwarding_services()` — Query catalog for forwarding services across all agents
- `get_nginx_configs()`, `watch_nginx_configs()` — KV operations for nginx configs
- `get_all_catalog_service_ids()` — Query the Consul catalog cluster-wide for all registered service instance IDs. Used by the nginx daemon GC to detect orphaned KV entries
- `delete_nginx_config_kv()` — Delete all nginx config and postproc KV entries for a service ID (both `sites/` and `streams/` prefixes)

### `daemon.rs` — DiscoveryDaemon

Orchestrates the full discovery lifecycle:

- `sync()` — Full reconciliation: sync running containers against config
- `handle_container_start()` — Match started container to network entries, allocate ports, register
- `handle_container_die()` — Deregister stale services
- `run_config_watcher()` — Watch `discovery.yaml` for file changes and re-sync

### `docker.rs` — Docker Client

Wraps bollard:
- `list_running_containers()` — List running containers with metadata
- `inspect_container()` — Inspect a single container by ID, returns all metadata

### `forwarding.rs` — Forwarding Rule Sync

Proxy-side DNAT management:
- `sync_forwarding_rules()` — Query Consul for forwarding services, apply DNAT rules via `lab-ops natmap dnat`

### `natmap.rs` — Natmap CLI Client

Calls `lab-ops natmap docker add/rm` as an external command via `std::process::Command`:
- `add_mapping()`, `remove_mapping()` — Manage Docker port mappings through natmap daemon

### `nginx_daemon.rs` — NginxDaemon

Proxy-side nginx config management:
- `sync()` — Pull configs from Consul KV, run postprocs, write to disk, reload nginx. Also runs a periodic GC sweep (every 5 min) that cross-references KV entries against the Consul catalog and deletes orphaned entries
- `run_loop()` — Blocking-query watch loop
- `gc_orphaned_kv_entries()` — GC sweep that finds and deletes KV entries whose service IDs no longer exist in the Consul catalog

### `ports.rs` — PortState

Manages ephemeral port allocation (32768-60999) with JSON persistence:
- `allocate()` — Assign unique port from pool
- `remove()` — Free a port
- `port_is_free()` — Check if a port is bound by any process

## `src/` (Root Crate)

### `cli.rs` — Top-Level CLI

Defines the root `Cli` struct and `Command` enum. Each variant delegates to a workspace crate via `#[command(flatten)]` or to a `src/cmd/` module.

### `cmd/cf2ansible.rs` — DNS Zone Converter

Converts BIND DNS zone files to Ansible YAML for Cloudflare DNS management.

### `cmd/dockernet.rs` — Docker Network Viewer

Displays Docker container IPs and port bindings in a formatted table.
