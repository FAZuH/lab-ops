# Architecture

## Project Layout

```
lab-ops/
├── src/                          # Main lab-ops binary
│   ├── main.rs                   # CLI entrypoint, command dispatch
│   ├── cli.rs                    # Clap CLI definitions (Command enum)
│   ├── consts.rs                 # Command name constants
│   ├── lib.rs                    # Library root
│   └── cmd/
│       ├── mod.rs                # Command module declarations
│       ├── cf2ansible.rs         # DNS zone → Ansible converter
│       ├── cf2terra.rs           # DNS zone → Terraform converter
│       ├── dns_parser.rs         # Shared DNS zone parser
│       └── dockernet.rs          # Docker network inspector
├── crates/
│   ├── lab-lib/                   # Shared utilities and types
│   │   └── src/
│   │       ├── lib.rs             # Module declarations
│   │       ├── consts.rs          # Shared constants (NATMAP_SOCKET, etc.)
│   │       ├── docker.rs          # Docker connect + name trimming helpers
│   │       ├── port.rs            # Port availability checking (SO_REUSEADDR, IP_FREEBIND)
│   │       └── protocol.rs        # TransportProtocol enum (TCP/UDP)
│   ├── natmap/                   # NAT management crate
│   │   └── src/
│   │       ├── lib.rs            # Module declarations
│   │       ├── cli.rs            # Command enum + CLI dispatch
│   │       ├── command.rs        # Handler functions for all subcommands
│   │       ├── daemon.rs         # Axum API server, state management
│   │       ├── models.rs         # Data types (configs, requests, state)
│   │       ├── iptables.rs       # IptablesManager (rule CRUD)
│   │       ├── port.rs           # PortAllocator (socket reservation)
│   │       ├── docker.rs         # Docker API client (bollard)
│   │       ├── install.rs        # Systemd service installation
│   │       └── utils.rs          # HTTP client for daemon communication
│   └── auto-discover/            # Service discovery daemon
│       └── src/
│           ├── lib.rs            # Module declarations
│           ├── cli.rs            # CLI definitions and dispatch
│           ├── config.rs         # YAML config parsing
│           ├── consul.rs         # Consul service registration
│           ├── daemon.rs         # Core discovery daemon
│           ├── docker.rs         # Docker API client
│           ├── forwarding.rs     # Proxy-side DNAT rule sync
│           ├── model.rs          # ContainerInfo data type
│           ├── natmap.rs         # Natmap client (CLI subprocess + HTTP)
│           ├── nginx_daemon.rs   # Nginx config generation watcher
│           └── port.rs           # Port assignment persistence + allocation
├── tests/                        # Integration tests
│   ├── integration.rs            # cf2ansible integration tests
│   └── natmap_docker.rs          # Docker-based NAT integration tests
├── docs/
│   ├── usage.md                  # User-facing documentation
│   ├── dev/                      # Developer documentation
│   └── diagrams/                 # Mermaid diagram sources (.mmd)
└── Cargo.toml                    # Workspace config
```

## Core Concepts

### Two-Mode Architecture

The `natmap` CLI operates in two modes depending on the subcommand:

**Daemon-dependent commands** (`dnat`, `snat`, `hairpin`, `docker add/rm/remap`, `ls`):
1. CLI parses arguments into a JSON request body
2. Sends HTTP request to the daemon via Unix socket (`/run/natmap.sock`)
3. Daemon performs port allocation, iptables management, and state persistence
4. Returns result to CLI

**Direct commands** (`fwd`, `save`, `daemon`, `install`):
1. CLI executes the operation directly (syscalls, iptables-save, etc.)
2. No daemon communication needed

### Why a Daemon?

The daemon is required because:
- **Port reservation**: Only a long-running process can hold sockets open to reserve ports
- **Crash recovery**: The daemon persists state, enabling clean restart after crashes
- **Docker integration**: Real-time Docker event monitoring requires a persistent process
- **Atomic rule management**: Port binding + iptables as a transactional unit

## Data Flow Diagram

See [../diagrams/crate-structure.png](../diagrams/crate-structure.png) for the module dependency graph.

See [../diagrams/cli-routing.png](../diagrams/cli-routing.png) for how CLI commands route to daemon API endpoints.

See [../diagrams/request-flow.png](../diagrams/request-flow.png) for the sequence of a DNAT rule add request.
