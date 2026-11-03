# Module Reference

## `crates/lab-lib/src/`

Shared utilities and types consumed by `natmap` and the root binary.

### `lib.rs` — Library Root

```rust
pub mod consts;
pub mod discovery;
pub mod docker;
pub mod port;
pub mod protocol;
```

### `protocol.rs` — TransportProtocol

The canonical `TransportProtocol` enum (`Tcp` / `Udp`) with serde, `Display`, and `TryFrom<String>`. Originally defined in natmap; extracted here so every crate can share it without depending on natmap.

### `consts.rs` — Shared Constants

| Constant | Value | Used By |
|---|---|---|
| `NATMAP_SOCKET` | `/run/natmap.sock` | natmap |
| `LAB_OPS_BIN` | `/usr/local/bin/lab-ops` | natmap install |
| `LAB_OPS_CMD` | `lab-ops` | completion helpers |

### `docker.rs` — Docker Helpers & Shared Inspection

The shared container-inspection contract, consumed by `natmap`:

- `connect()` — Connects to the local Docker daemon via `Docker::connect_with_local_defaults()`
- `trim_container_name(name) -> &str` — Strips the leading `/` from Docker container names
- `DockerClient` — Bollard client wrapper with `new()`, `list_running_containers()`, and `inspect_container(container_id)`. Both container methods return the shared `ContainerInfo` shape
- `ContainerInfo {id, name, compose_project, ip, networks}` — The shared container shape. `ip` is the primary container IP: the first attached network (sorted by name) with an address. `networks` holds the per-network settings
- `ContainerNetwork {name, ip, gateway}` — The settings for one network a container is attached to
- `parse_container_inspect(inspect) -> ContainerInfo` — Parses a `ContainerInspectResponse` into the shared container shape
- `parse_port_mappings(inspect) -> Vec<PortMapping>` — Parses the published port mappings from an inspect response

### `discovery.rs` — `discovery.yaml` Model

The declaration model for a node's published services, consumed by
`natmap::apply`:

- `DiscoveryConfig {node, defaults, services}` — root of the file. `load(path)` reads and deserializes it. Unknown keys are ignored, so a declaration may carry blocks this model does not know (`nginx:`) and still load
- `NodeConfig {name, tailnet_ip}` — the node's name and the address other nodes reach it on
- `Defaults` — values a service inherits when it does not set them (`proxy_on`, `proxy_ip`, `bind_ip`, `bind_interface`, `preserve_src_ip*`)
- `ServiceConfig` — one `services:` entry: `type` (`docker` / `local`), optional `match` block, `address`, `bind_ip`, `bind_interface`, the owned ports (`ports`, the ownership ledger), and the port registrations (`rproxylocal`, `rproxyremote`, `forwardlocal`, `forwardremote`)
- `PortClaim {port, bind_ip}` — one host port a service owns, and the address it answers on
- `default_bind_ip()` — `defaults.bind_ip`, else `DEFAULT_BIND_IP` (`127.0.0.1`); the consumer renderer carries the same default
- `port_claims() -> Vec<(&str, &PortClaim)>` — every owned port with its service, in port order
- `MatchConfig` — container criteria. `matches(container)` requires every criterion set; an invalid `container_regex` matches nothing
- `ServiceConfig::matches_container(container)` — a service without a `match` block covers every container
- `resolve_all() -> Vec<ResolvedService>` — expands every service into one resolved registration per port, with defaults applied
- `ResolvedService` / `ResolvedPortType` — the flattened result `apply_service` consumes. `primary_domain()`, `domain_slug()`, and `domains()` are rproxy helpers

### `port.rs` — Port Utilities & Management

Two layers:

**Low-level socket utilities:**
- `create_freebind_socket(addr, Type) -> io::Result<Socket>` — Creates a `socket2::Socket` with `SO_REUSEADDR`, `IP_FREEBIND`, and the given socket type (`STREAM` for TCP, `DGRAM` for UDP).

**`PortAllocator`** — Runtime TCP/UDP pre-bind reservation (used by `natmap` daemon):
- Holds `HashMap<SocketAddr, ReservedSocket>` where `ReservedSocket` is an enum with `Tcp(TcpListener)` and `Udp(UdpSocket)` variants.
- `allocate(addr, proto)` — Binds `addr` with protocol-appropriate socket type (`STREAM` + `listen()` for TCP, `DGRAM` for UDP); stores the reservation
- `deallocate(addr)` — Remove from map, drop reservation (releases port)
- `is_allocated(addr)` — Check if `addr` has an active reservation
- `deallocate_all()` — Clear all reservations

## `crates/natmap/src/`

### `lib.rs` — Library Root

Declares all modules with `pub` visibility:

```rust
pub mod api;
pub mod apply;
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

### `apply.rs` — Apply a Declaration

Two entry points over one daemon seam, with no Consul involvement:

**Declarative apply** (`natmap plan` / `natmap apply`):

- `NatmapApply` — seam over the daemon's `add_mapping`, `list_mappings`, `remove_mapping` and `policy_route`; `NatmapClient` is the production implementation, tests fake it
- `declared_mappings(config) -> (Vec<DeclaredMapping>, Vec<CoveredPort>)` — one mapping per declared port, from the node's tailnet address to the address the port answers on. A port already on the tailnet address or on a wildcard is covered instead. Refuses a missing `tailnet_ip`, a port claimed twice, and port 0
- `plan(declared, covered, live, prune) -> Plan` — the diff: `create`, `update`, `remove`, `keep` (hand-added, never pruned), `stale` (owned, left without `--prune`), `covered`, `conflicts`. `Plan::drift_message()` / `conflict_message()` word the report
- `converge(apply, config, options) -> Result<Converged>` — refuses conflicts before writing (`Converged::Refused(plan)`, nothing written), then releases handed-over ports, creates and updates, then releases the stale ones (`Converged::Applied(plan)`). `ConvergeOptions {prune, dry_run}`. A declared mapping is written under `service_mapping_key(service)`, never under the bare service name
- `Drift` — the marker `plan` and `apply --dry-run` return when the node does not match; `main.rs` turns it into exit 1 without an error report

**Per-service apply** (the older whole-node path):

- `ApplyTarget {id, container_ip, local_ip}` — what a resolved service points at (a container ID, or the service key for a local service)
- `decide_ports(resolved, local_target) -> Option<PortDecision>` — explicit `bind_port`/`ext_ports[0]`, or `host_port: 0` for daemon allocation; `None` when an rproxy service on a local target needs no mapping
- `natmap_bind_ip(resolved)` — `bind_ip`, else the first IPv4 address of `bind_interface`, else the daemon's `0.0.0.0`
- `service_address(resolved, target)` — the node-side address used as the policy-route source fallback
- `apply_service(apply, resolved, target) -> Result<u16>` — decide the port, create the mapping (returning the daemon-reported host port), then install the `preserve_src_ip` policy route when the service sets one. An explicit port survives a `Conflict`, `NotFound`, or `Unavailable` response; a rejected dynamic request fails
- `resolve_interface_ip(iface)` — first IPv4 address from `ip -j -4 addr show`

### `converge.rs` — One Convergence Pass

The tick `apply` runs, and the timer runs through it:

- `tick(apply, declaration, options) -> Result<Tick>` — re-reads the declaration from disk (the file a tick must never cache), converges through the same `apply::converge`, and logs one event. A declaration that cannot be read is an error and writes nothing
- `Tick { declaration, converged }` — what the pass read and did. `plan()` and `was_converged()` are the accessors the CLI prints from
- `record(tick, dry_run)` — one canonical log line: at `info` with the change counts when something changed, at `debug` when nothing did

No loop, cursor, or "last applied" marker lives here: scheduling is systemd's (`natmap-converge.timer`), and the daemon's mappings are the only state a tick reads.

### `client.rs` — Typed Client

`pub struct NatmapClient { socket: PathBuf }` — one typed method per daemon operation, speaking the same HTTP-over-Unix-socket protocol as the CLI's `request_json`:

- Static NAT ops take the typed config struct + an explicit `delete: bool` and return `Result<Option<T>, NatmapError>` (daemon echo on install, `None` on delete): `dnat(DnatConfig, bool)`, `hairpin`, `policy_route`.
- Docker mapping ops mirror the daemon endpoints: `add_mapping(&str, DockerAddMapRequest) -> Result<DockerPortMap>`, `mappings() -> Result<Vec<DockerPortMap>>` (the daemon's persisted mappings, with their owners), `remove_mapping(&str, u16)` (a mapping the daemon no longer holds is already released, so `NotFound` is success), and `rules() -> Result<Vec<LiveRule>>` (live rules parsed from `iptables-save`).
- `new(impl Into<PathBuf>)` and `default_socket()` (env `NATMAP_SOCKET`, else `lab_ops_lab_lib::NATMAP_SOCKET`). Re-exports `NatmapError` from `utils.rs`. Consumed by `apply` and other callers that use the config structs directly rather than building `cli::Cli` values.

### `cli.rs` — CLI Definitions

Defines the `Command` enum with clap derives. Each variant maps to a subcommand:

```rust
pub enum Command {
    Dnat { ext_ip, int_ip, proto, ports, ext_if, delete },
    Snat { int_ip, ext_if, ext_ip, delete },
    Hairpin { ext_ip, int_ip, proto, ports, lan_cidr, delete },
    List { container_id },
    Plan { from, prune },
    Apply { from, prune, dry_run },
    Docker { cmd: Docker },
    Save,
    Fwd,
    Daemon { state_dir, socket, socket_group },
    Install { group, binary, from, interval },
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

`handle_plan` and `handle_apply` run `converge::tick` — the module the timer
also runs, so the CLI and the timer cannot diverge — and print the resulting
`Plan` as a table (or JSON with `--json`) through `print_plan`. `plan` and
`apply --dry-run` return `apply::Drift` after printing, which the binary
entrypoint maps to exit 1; a refusal prints first and then fails.

### `daemon.rs` — API Server & State

The largest module. Contains:

- **`AppState`**: Shared state (DaemonState, IptablesManager, PortAllocator, Docker client, next_id counter)
- **`Daemon::run()`**: Startup sequence (setup, reload, Docker maintenance task, graceful shutdown)
- **`reload()`**: Crash recovery — flushes stale rules, rebinds from state.json; when Docker is unreachable, reinstalls persisted mappings from their stored addresses
- **`AppState::persist()`**: Atomic write of DaemonState to state.json (via temp file + rename)
- **`maintain_docker()`**: Docker maintenance task — reconnects with capped backoff, reloads, and resubscribes the event stream
- **`listen_docker_events()`**: Docker event stream handler for container start/die events
- **API handlers**: `add_dnat`, `remove_dnat`, `add_snat`, `remove_snat`, `add_hairpin`, `remove_hairpin`, `add_mapping`, `remove_mapping`, `remove_mapping_by_id`, `remap_port`, `list_mappings`
- **`reconcile_persisted_entries`**: the persisted half of a reload, against a set of running container ids and their current addresses. A `service:` entry is replayed and kept whatever Docker reports; a container entry is kept only while its container runs. `reconcile_live_portmaps` adds the inspect and untracked-container discovery around it
- **`reserve_mapping_addr`**: the one place a mapping takes its host address — a claim for a locally delivered target, a bound listener otherwise

### `models.rs` — Data Types

Key types:

| Type | Purpose |
|------|---------|
| `DnatConfig` | Persisted DNAT rule (ext_ip, int_ip, ports, proto, ext_if, preserve_src_ip). `targets_local_delivery()` reports a loopback destination |
| `SnatConfig` | Persisted SNAT rule (int_ip, ext_ip, ext_if) |
| `HairpinConfig` | Persisted hairpin rule (ext_ip, int_ip, ports, proto, optional lan_cidr) |
| `DaemonState` | Top-level persisted state (docker, dnats, snats, hairpins) |
| `ListResponse` | API response for `GET /mappings` |
| `DockerPortMap` | Running Docker mapping (id, request, container info, comment, `owner` — the service the apply that created it recorded, `None` when added by hand) |
| `is_locally_delivered(addr)` | Whether a DNAT destination is on this host: `input` hook, kernel rule alone, no userspace listener. `DnatConfig::targets_local_delivery()` and `DockerPortMapRequest::targets_local_delivery()` both delegate to it |
| `SERVICE_KEY_PREFIX` / `service_mapping_key` / `is_service_mapping_key` | The `service:` namespace that keeps a declared mapping's state key from being mistaken for a container id |
| `DockerPortMapRequest` | Docker mapping config (host_addr, container_addr, proto) |
| `DockerAddMapRequest` / `DockerRemapRequest` | Docker API request bodies. `DockerAddMapRequest::owner` carries the ledger entry |

The `TransportProtocol` enum is re-exported from `lab_lib::TransportProtocol`.

### `iptables.rs` — IptablesManager

Stateless manager for iptables operations. Key methods:

| Method | Purpose |
|--------|---------|
| `setup()` | Create NATMAP chains and jump rules in filter/nat tables |
| `flush_all_natmap()` | Flush ALL rules in NATMAP chains (crash recovery) |
| `install_dockermap()` | Install Docker port mapping (DNAT + accept + MASQUERADE + OUTPUT) |
| `remove_mapping()` | Remove Docker mapping by comment |
| `install_dnat()` / `remove_dnat()` | Static DNAT rules. The accept lands in `INPUT` when the destination is loopback (locally delivered), in `FORWARD` otherwise; a loopback rule is installed with `ip6tables` when the destination is IPv6 |
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
4. With a declaration, also writes `natmap-converge.service` + `natmap-converge.timer` and enables the timer. `render_converge_units(binary, declaration, interval)` is the pure renderer; `DEFAULT_CONVERGE_INTERVAL_SECS` is the default tick interval
4. Writes `/etc/systemd/system/natmap.service`
5. Runs `systemctl daemon-reload` and `systemctl enable --now natmap`

### `utils.rs` — HTTP Client

HTTP client for daemon communication:
- `NatmapError` — typed error enum (variants: `Connect`, `Http`, `Json`, `BadRequest`, `NotFound`, `Conflict`, `Internal`, `Unavailable`, `UnexpectedStatus { status, body }`). Hand-implemented `Display` + `std::error::Error`; status → variant via `from_status()`. This is the one deliberate exception to the "no custom error enums" rule (see `docs/dev/standards.md` §4).
- `request_json<T, R>(socket_path, method, path, body) -> Result<T, NatmapError>` — Sends HTTP request to Unix socket, deserializes JSON response. Maps non-success statuses to `NatmapError` variants instead of a flat error string.

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
