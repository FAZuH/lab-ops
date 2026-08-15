# Module Reference

## `crates/lab-lib/src/`

Shared utilities and types consumed by both `natmap` and `auto-discover`.

### `lib.rs` — Library Root

```rust
pub mod consts;
pub mod docker;
pub mod port;
pub mod protocol;
```

### `protocol.rs` — TransportProtocol

The canonical `TransportProtocol` enum (`Tcp` / `Udp`) with serde, `Display`, and `TryFrom<String>`. Originally defined in natmap; extracted here so auto-discover can use it without depending on natmap.

### `consts.rs` — Shared Constants

| Constant | Value | Used By |
|---|---|---|
| `NATMAP_SOCKET` | `/run/natmap.sock` | natmap, auto-discover |
| `LAB_OPS_BIN` | `/usr/local/bin/lab-ops` | auto-discover |
| `LAB_OPS_CMD` | `lab-ops` | auto-discover |

### `docker.rs` — Docker Helpers

- `connect()` — Connects to the local Docker daemon via `Docker::connect_with_local_defaults()`
- `trim_container_name(name) -> &str` — Strips the leading `/` from Docker container names

### `port.rs` — Port Utilities & Management

Consolidated from the old `natmap/src/port.rs` and `auto-discover/src/port.rs`. Three layers:

**Low-level socket utilities:**
- `create_freebind_socket(addr, Type) -> io::Result<Socket>` — Creates a `socket2::Socket` with `SO_REUSEADDR`, `IP_FREEBIND`, and the given socket type (`STREAM` for TCP, `DGRAM` for UDP).
- `is_port_free(addr: A) -> bool` where `A: ToSocketAddrs` — Checks if a TCP port is free using the robust freebind socket. Generic over any type that can resolve to a socket address.

**`PortAllocator`** — Runtime TCP/UDP pre-bind reservation (used by `natmap` daemon):
- Holds `HashMap<SocketAddr, ReservedSocket>` where `ReservedSocket` is an enum with `Tcp(TcpListener)` and `Udp(UdpSocket)` variants.
- `allocate(addr, proto)` — Binds `addr` with protocol-appropriate socket type (`STREAM` + `listen()` for TCP, `DGRAM` for UDP); stores the reservation
- `deallocate(addr)` — Remove from map, drop reservation (releases port)
- `is_allocated(addr)` — Check if `addr` has an active reservation
- `deallocate_all()` — Clear all reservations

**`PortAssignments`** — Persistent ephemeral port allocation (used by `auto-discover`):
- `get(key) / set(key, port) / remove(key)` — CRUD for port assignments
- `load(path) / save(path)` — Persist assignments to JSON (for crash recovery)
- `get_or_allocate(key)` — Look up existing assignment or allocate from ephemeral range
- `allocate_port(assignments)` — Find first free port in range 32768-61000 using `is_port_free`

## `crates/natmap/src/`

### `lib.rs` — Library Root

Declares all modules with `pub` visibility:

```rust
pub mod api;
pub mod cli;
pub mod client;
pub mod command;
pub mod completions;
pub mod consts;
pub mod daemon;
pub mod docker;
pub mod install;
pub mod iptables;
pub mod models;
pub mod policy_route;
pub mod utils;
```

### `client.rs` — Typed Client

`pub struct NatmapClient { socket: PathBuf }` — one typed method per daemon operation, speaking the same HTTP-over-Unix-socket protocol as the CLI's `request_json`:

- Static NAT ops take the typed config struct + an explicit `delete: bool` and return `Result<Option<T>, NatmapError>` (daemon echo on install, `None` on delete): `dnat(DnatConfig, bool)`, `snat`, `hairpin`, `policy_route`.
- Docker mapping ops mirror the daemon endpoints: `add_mapping(&str, DockerAddMapRequest) -> Result<DockerPortMap>`, `remove_mapping(&str, u16)`, `remove_mapping_by_id(u64)`, `remap_port(&str, DockerRemapRequest) -> Result<Vec<DockerPortMap>>`, `list_mappings() -> Result<ListResponse>`, `clear()`.
- `new(impl Into<PathBuf>)` and `default_socket()` (env `NATMAP_SOCKET`, else `lab_ops_lab_lib::NATMAP_SOCKET`). Config → request conversion happens inside the client via `From` impls in `models.rs`. Re-exports `NatmapError` from `utils.rs`. Consumed by auto-discover (which previously built `cli::Cli` values and called `run_cli`).

### `cli.rs` — CLI Definitions

Defines the `Command` enum with clap derives. Each variant maps to a subcommand:

```rust
pub enum Command {
    Dnat { ext_ip, int_ip, proto, ports, ext_if, delete },
    Snat { int_ip, ext_if, ext_ip, delete },
    Hairpin { ext_ip, int_ip, proto, ports, lan_cidr, delete },
    List { container_id },
    Docker { cmd: Docker },
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

Also defines the top-level `Cli` struct with global `--socket`, `--json`, and `--color` flags. The `run_cli(cli, use_color)` function dispatches each variant to the appropriate handler in `command.rs`, passing the color preference for table styling.

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
| `DnatConfig` | Persisted DNAT rule (ext_ip, int_ip, ports, proto, ext_if, preserve_src_ip) |
| `SnatConfig` | Persisted SNAT rule (int_ip, ext_ip, ext_if) |
| `HairpinConfig` | Persisted hairpin rule (ext_ip, int_ip, ports, proto, optional lan_cidr) |
| `DnatRequest` / `SnatRequest` / `HairpinRequest` | API request bodies |
| `DaemonState` | Top-level persisted state (docker, dnats, snats, hairpins) |
| `ListResponse` | API response for `GET /mappings` |
| `DockerPortMap` | Running Docker mapping (id, request, container info, comment) |
| `DockerPortMapRequest` | Docker mapping config (host_addr, container_addr, proto) |
| `DockerAddMapRequest` / `DockerRemapRequest` | Docker API request bodies |

The `TransportProtocol` enum is re-exported from `lab_lib::TransportProtocol`.

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
| `install_hairpin()` / `remove_hairpin()` | Static hairpin rules. When `lan_cidr` is set on the config, skips PREROUTING DNAT and uses the LAN subnet as the MASQUERADE source match instead of `0.0.0.0/0` |
| `flush_container_rules()` | Remove all rules for a container |

<!-- PortAllocator moved to lab_lib::port; see port.rs section in lab-lib crate above -->

### `docker.rs` — Docker Client

Wraps the `bollard` Docker API crate:
- `connect()` — Creates a bollard Docker client (delegates to [`lab_lib::docker::connect()`])
- `get_port_mappings()` — Inspects a container via bollard, then delegates parsing to [`lab_lib::docker::parse_container_inspect`] and [`lab_lib::docker::parse_port_mappings`]. Converts each shared `PortMapping` into a natmap `DockerPortMap`. Preserves the `"unknown"` name fallback. Keeps the passed container id for the rule comment (`natmap:<container_id>:<host_port>`)

### `install.rs` — Systemd Installer

`install_systemd()` function that:
1. Copies the binary to the target path
2. Creates a `natmap` system group
3. Renders the systemd service template (substitutes `{binary}`, `{state_dir}`, `{group}`)
4. Writes `/etc/systemd/system/natmap.service`
5. Runs `systemctl daemon-reload` and `systemctl enable --now natmap`

### `utils.rs` — HTTP Client

HTTP client for daemon communication:
- `NatmapError` — typed error enum (variants: `Connect`, `Http`, `Json`, `BadRequest`, `NotFound`, `Conflict`, `Internal`, `Unavailable`, `UnexpectedStatus { status, body }`). Hand-implemented `Display` + `std::error::Error`; status → variant via `from_status()`. This is the one deliberate exception to the "no custom error enums" rule (see `docs/dev/standards.md` §4).
- `request_json<T, R>(socket_path, method, path, body) -> Result<T, NatmapError>` — Sends HTTP request to Unix socket, deserializes JSON response. Maps non-success statuses to `NatmapError` variants instead of a flat error string.

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
mod model;
```

### `cli.rs` — CLI Definitions

Defines `Cli` struct and `Commands` enum with clap derives. The `run_cli(cli, _use_color)` function dispatches each variant. Subcommands:

| Variant | Purpose |
|---|---|
| `Daemon` | Unified long-running daemon (discovery + forwarding) |
| `Sync` | One-shot discovery sync pass |
| `Check` | Validate `discovery.yaml` |
| `ForwardingSync` | One-shot proxy-side DNAT rule sync |

### `config.rs` — DiscoveryConfig

Parses `/etc/auto-discover/discovery.yaml`:

- **`DiscoveryConfig`**: Top-level config (`name`, `bind_ip`, `bind_interface`, `networks`, `defaults`)
- **`NetworkEntry`**: Per-network config (`name`, `container_port`, `protocol`, `template`, `forwarding`)
- **`ResolvedService`**: Fully resolved config with all defaults applied

### `consul.rs` — ConsulClient

Interacts with Consul Agent API:

- `register_service()` — Register/update a service with metadata
- `deregister_by_container()` — Remove all services matching a container ID
- `get_services_by_container()` — Query services by container ID
- `get_forwarding_services()` — Query catalog for forwarding services across all agents
- `get_all_catalog_service_ids()` — Query the Consul catalog cluster-wide for all registered service instance IDs
- `delete_kv()` — Delete a single KV entry

### `daemon.rs` — DiscoveryDaemon

Orchestrates the full discovery lifecycle:

- `sync()` — Full reconciliation: sync running containers against config
- `handle_container_start()` — Match started container to network entries, allocate ports, register
- `handle_container_die()` — Deregister stale services
- `run_config_watcher()` — Watch `discovery.yaml` for file changes and re-sync
- `ensure_docker_mapping()` — Adds a Docker mapping via `lab_ops_natmap::client::NatmapClient`, swallowing `NatmapError::Conflict`/`NotFound` (mapping already exists / container missing) as non-fatal

### `docker.rs` — Docker Client

Wraps bollard:
- `list_running_containers()` — List running containers with metadata
- `inspect_container()` — Inspect a single container by ID, returns all metadata
- `get_container_ip(container_id)` — Free function running `docker inspect` for a container's first network IP (fallback when no `bind_ip`/`bind_interface` is configured); `parse_docker_inspect_output` is its pure parsing half

### `forwarding.rs` — Forwarding Rule Sync

Proxy-side DNAT management:
- `sync_forwarding_rules()` — Query Consul for forwarding services, apply DNAT rules via `lab_ops_natmap::client::NatmapClient`
- `parse_group_proto(proto)` — Parses a forwarding group's protocol string, rejecting invalid values so a misconfigured service fails the sync instead of silently installing a TCP rule
- `get_lan_cidr(ip)` — Determine the LAN subnet CIDR containing the given IP by querying the routing table (used for LAN-limited hairpin when `preserve_src_ip` is true)

<!-- PortAssignments moved to lab_lib::port; see port.rs section in lab-lib crate above -->

## `src/` (Root Crate)

### `cli.rs` — Top-Level CLI

Defines the root `Cli` struct with `--verbose` and `--color` global flags, and the `Command` enum with a `Completions` variant. Each other variant delegates to a workspace crate via `#[command(flatten)]` or to a `src/cmd/` module.

### `main.rs` — Entrypoint

Initializes tracing with verbosity and color from CLI args, then dispatches to the selected command. Also contains the `generate_completions()` helper and `completion_filename()` for the `completions` subcommand.

### `consts.rs` — CLI Constants

Command name constants (`CMD_*`) used across the root binary, natmap subprocess invocations, and output headers.

### `cmd/cf2ansible.rs` — DNS Zone → Ansible Converter

Converts BIND DNS zone files to Ansible YAML for Cloudflare DNS management using `community.general.cloudflare_dns`.

### `cmd/cf2terra.rs` — DNS Zone → Terraform Converter

Converts BIND DNS zone files to Terraform `cloudflare_record` resource blocks. Supports `A`, `AAAA`, `CNAME`, `MX`, `TXT`, `SRV`, `NS`. Accepts `--zone-id-var` for the Terraform zone ID variable reference.

### `cmd/dns_parser.rs` — Shared DNS Zone Parser

Parses BIND zone files into `DnsRecord` structs used by both `cf2ansible` and `cf2terra`. Handles `A`, `AAAA`, `CNAME`, `MX`, `TXT`, `SRV`, `TLSA`, `NS` record types and Cloudflare proxy annotations (`; cf_tags=cf-proxied:true|false`).

### `cmd/dockernet.rs` — Docker Network Viewer

Displays Docker container IPs and port bindings in a formatted table. Accepts a `use_color: bool` parameter to control header/status coloring.
