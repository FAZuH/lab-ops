# Codebase Standards

This document defines the canonical coding standards for the `lab-ops` workspace. All code must follow these rules. Where existing code violates a standard, that code is a backlog item to be refactored — new code must comply.

## 1. Rust Edition

**Every crate in the workspace must use edition `"2024"`.**

- `Cargo.toml` field: `edition = "2024"`


## 2. Module Structure

### 2.1. Crate Root

Every workspace crate must have a `src/lib.rs` that declares all modules with `pub` visibility only when the module is part of the crate's public API.

```rust
// lib.rs — canonical form
pub mod cli;           // exported: consumed by the root binary
mod config;            // internal
mod consul;
mod daemon;
// ...
```

### 2.2. Subcommand Implementations

The root `lab-ops` crate uses `src/cmd/` for its own inline subcommands:

```
src/
├── lib.rs              // exports cli + cmd
├── cli.rs              // top-level Cli struct + Command enum
├── main.rs             // entrypoint: parse, dispatch
└── cmd/
    ├── mod.rs
    ├── cf2ansible.rs
    └── dockernet.rs
```

Workspace crates (`natmap`, `auto-discover`) keep their full CLI definitions inside their own `src/` and are integrated into the root CLI via `#[command(flatten)]`.

### 2.3. File Naming

- Module files: `snake_case.rs` (e.g., `nginx_daemon.rs`, `port_allocator.rs`)
- Test-only helper modules (if shared across test files) go in `tests/common/`


## 3. Naming Conventions

### 3.1. CLI Enum

- **Name**: `Command` (singular, unprefixed)
- **Variants**: `PascalCase`

```rust
pub enum Command {
    Cf2Ansible { zone_file: PathBuf, zone_name: Option<String> },
    ForwardingSync { consul_addr: String },
}
```

### 3.2. CLI Subcommand String Names

Use constants in the root crate's `cli.rs` for subcommand name strings. Workspace crates may use inline strings or follow the same pattern.

```rust
pub const CMD_FORWARD: &str = "forward";
```

### 3.3. Structs, Enums, Traits

- `PascalCase`, descriptive
- Error types: `XxxError` (e.g., `ConsulError`, `ConfigError`)
- Request/response types: `XxxRequest`, `XxxResponse` (e.g., `DnatRequest`)

### 3.4. Functions and Methods

- `snake_case`, verb-first for actions, noun-first for queries
  - `sync_forwarding_rules()`, `handle_container_start()`, `build_consul_service()`

### 3.5. Constants

- `UPPER_SNAKE_CASE`

### 3.6. Test Functions

All test functions must follow one convention:

```
<module_or_function>_<scenario>
```

| Part | Convention |
|---|---|
| `<module_or_function>` | The thing under test (snake_case) |
| Separator | Single underscore `_` |
| `<scenario>` | What is being tested (snake_case, descriptive) |

Examples:
```
parse_minimal_config
parse_full_config
resolve_overrides_default
allocate_port_assigns_unique
format_ips_single
format_ips_none
build_consul_service_with_forwarding
build_consul_service_udp_check
```




## 4. Error Handling

### 4.1. Use `color_eyre` Throughout

**Every crate in the workspace must use `color_eyre`.** There are no custom error enums in this project. The root `main.rs` calls `color_eyre::install()?` at startup.

```rust
use color_eyre::eyre::Result;
use color_eyre::eyre::{bail, eyre};

fn example() -> Result<()> {
    if something_wrong {
        bail!("descriptive error: {details}");
    }
    Ok(())
}
```

### 4.2. Custom Error Types → Delete

Replace custom error enums (`ConfigError`, `DaemonError`, `ConsulError`, etc.) in `auto-discover` with `color_eyre::Result` and `bail!()` / `eyre!()`.

### 4.3. No `unwrap()` / `expect()` / `process::exit()`

- `unwrap()` and `expect()` are only allowed in `LazyLock<Regex>` statics (compile-time known-valid patterns).
- `process::exit()` is never called from library code. Only the binary entrypoint (`main.rs`) may exit the process.
- Prefer `?` and `bail!()`.

### 4.4. `run_cli` Function Signature

All `run_cli` functions must:

- Return `color_eyre::Result<()>`
- NOT call `process::exit()` internally
- Let the caller (root `main.rs`) handle the exit code
- Accept a `use_color: bool` parameter for table styling

```rust
pub async fn run_cli(cli: Cli, use_color: bool) -> Result<()> { ... }
```


## 5. Documentation

### 5.1. Module-Level Docs

**Every `.rs` file must start with a `//!` module-level doc comment** describing the module's purpose and contents.

```rust
//! TCP socket-based port reservation.
//!
//! The [`PortAllocator`] binds a `TcpListener` to each allocated port
//! to prevent the kernel from reassigning it.
```

### 5.2. Public Item Docs

**Every `pub` item must have a `///` doc comment.** This includes:
- Public structs and enums
- Public functions and methods
- Public constants
- Public type aliases

Minimum: one sentence describing what it does. Preferred: description + example for non-trivial items.

### 5.3. Section Comments

Use `// --- Section Name ---` delimiters to separate logical sections within large files (>200 lines). This is mandatory for `daemon.rs`, `api.rs`, `command.rs`.

```rust
// --- Static NAT handlers ---

pub async fn add_dnat(...) -> Result<()> { ... }

// --- Docker handlers ---

pub async fn add_mapping(...) -> Result<()> { ... }
```

### 5.4. No Redundant/Obvious Comments

- Do NOT comment what code literally says: `// increment counter` above `i += 1`
- Do NOT comment obvious parameter names in struct fields
- Comments explain *why*, not *what*


## 6. Imports

### 6.1. No Glob Imports

`use crate::models::*;` is forbidden. Import each item explicitly.

```rust
// Good
use crate::models::{DnatConfig, DaemonState, PortMappingRequest};

// Forbidden
use crate::models::*;
```

### 6.2. No Redundant Module Imports

```rust
use crate::consul::ConsulClient;  // specific item
use crate::consul;                // FORBIDDEN: redundant module import
```

If you need the module itself (e.g., `consul::SomeType`), import only the module. If you need specific items, import only those items. Not both.

### 6.3. No Trait Imports as `_`

```rust
use clap::Parser as _;   // FORBIDDEN
```

Unused trait imports should be removed. If a trait is needed for its methods, import it by name.


## 7. Testing

### 7.1. Test Location

| What to test | Where |
|---|---|
| Pure logic (parsing, formatting, data transforms) | Inline: `#[cfg(test)] mod tests { }` in the same file |
| Module integration (multiple modules together) | `tests/<name>.rs` in the crate's `tests/` directory |
| Docker-dependent | Behind `#[cfg(feature = "docker-tests")]` in the relevant test file |

### 7.2. Test Naming

See §3.6 — `<module_or_function>_<scenario>`.

### 7.3. Test Structure

Each test module must follow this skeleton:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Tests go here
}
```

### 7.4. Test Fixtures

- Simple data: `const` or inline literals at the top of `mod tests`
- Reusable constructors: private helper functions prefixed with `make_` (e.g., `make_networks()`, `make_resolved_service()`)
- Filesystem state: use `tempfile::TempDir` from dev-dependencies
- Docker state: use the shared harness in `tests/natmap_docker.rs`

### 7.5. Coverage Goal

Every non-trivial public function must have at least one test. "Happy path" coverage is the minimum. Edge cases (empty input, error paths) are expected for parsing/serialization code.

### 7.6. Required Tests by Module Type

| Module type | Minimum tests |
|---|---|
| CLI parsing (`cli.rs`) | At least one test per subcommand variant |
| Data models (`models.rs`) | Serialization round-trip for each request/response type |
| Parsing/formatting | Edge cases: empty, maximum, invalid inputs |
| HTTP handlers | At minimum: request construction is correct |


## 8. Async & Runtime

### 8.1. Tokio Runtime

- The root `main.rs` creates the tokio runtime. Subcommands are called via `rt.block_on()`.
- Workspace crate CLI entry functions (`run_cli`) are `async fn` and must NOT create their own runtime.
- Long-lived daemon loops (Docker events, Consul blocking queries) run on a multi-thread runtime: `Builder::new_multi_thread()`.

### 8.2. Runtime Builder Pattern

```rust
use tokio::runtime::Builder;

let rt = Builder::new_multi_thread()
    .enable_all()
    .build()?;
rt.block_on(async_fn())?;
```

For quick subcommands that don't need parallelism, `new_current_thread()` is acceptable.

### 8.3. No `#[tokio::main]` in Workspace Crates

Workspace crates are libraries. Their `run_cli` functions are `async fn` — never decorated with `#[tokio::main]`. The root binary owns the runtime.


## 9. Tracing / Logging

### 9.1. Use `tracing` Crate

**All crates use `tracing` for logging.** `println!()` / `eprintln!()` are only for user-facing output (e.g., `check_config()` output, CLI tables).

```rust
use tracing::{info, warn, error, debug, trace};
```

### 9.2. Subscriber Initialization

- The root `main.rs` initializes the tracing subscriber ONCE before dispatching subcommands.
- Workspace crate entry functions must NOT initialize tracing.
- Use `RUST_LOG` env var for filtering. Default: `info`.

```rust
tracing_subscriber::fmt()
    .with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
    )
    .init();
```

### 9.3. Log Levels

| Level | Use case |
|---|---|
| `error!` | Operation failed, needs attention |
| `warn!` | Recoverable problem, will retry |
| `info!` | Key lifecycle events (start, stop, sync complete) |
| `debug!` | Per-event detail (container start, rule added) |
| `trace!` | Very detailed (per-request, per-field) |

### 9.4. Structured Fields

**No string interpolation in messages.** The message is a static label. All variable data goes in fields.

```rust
// WRONG
info!("Docker event received: {} on container {}", action, id);

// CORRECT
info!(
    container.id = %id,
    container.name = %name,
    event.action = %action,
    event.type   = %typ,
    "docker event received"
);
```

Field sigils:
- `%value` — Display (strings, IDs, IPs, ports)
- `?value` — Debug (structs/enums you don't control)
- bare value — primitives (bool, u16, etc.)

Standard field names:
- `container.id`
- `container.name`
- `event.action`
- `consul.svc_id`
- `consul.addr`
- `host.addr`
- `host.port`
- `container.addr`
- `container.port`
- `ext.ip`
- `int.ip`
- `proto`
- `rule.count`
- `config.count`
- `mappings.count`
- `dnats.count`
- `daemon`
- `socket.path`

### 9.5. Span Hierarchy

Every async function that handles a Docker event, processes a Consul operation, or applies an iptables rule MUST be wrapped in a span using `#[instrument(skip_all, fields(...))]` or `.instrument(span)`.

**auto-discover hierarchy:**
```
event_loop              ← long-lived, fields: daemon="auto-discover"
  handle_container_start  ← per-event, fields: container.id, event.action, compose.project
    register_consul         ← fields: consul.svc_id, consul.addr
    add_natmap_mapping      ← fields: host.port, container.port, proto
  handle_container_die    ← per-event, fields: container.id, event.action
    deregister_consul       ← fields: consul.svc_id
  sync_forwarding_rules   ← fields: rule.count
  sync_nginx_configs      ← fields: config.count
```

**natmap hierarchy:**
```
daemon                  ← long-lived, fields: daemon="natmap", socket.path
  handle_docker_event     ← per-event, fields: container.id, event.action
  reload                  ← fields: mappings.count, dnats.count
  add_dnat / remove_dnat  ← fields: ext.ip, int.ip, ports, proto
  add_mapping             ← fields: host.addr, container.addr, proto, container.id
```

## 10. Dependency Management

### 10.1. Version Specifiers

All crates that share a dependency must use the same version specifier. Prefer `"major.minor"` (e.g., `"0.12"`) for stability, pin to full semver (`"1.52.2"`) only when a specific patch is required.

| Dependency | Specifier |
|---|---|
| `tokio` | `"1"` with `features = ["full"]` |
| `serde` | `"1"` with `features = ["derive"]` |
| `clap` | `"4"` with `features = ["derive"]` |
| `reqwest` | `"0.12"` with `features = ["json"]` |
| `color_eyre` | `"0.6"` |

### 10.2. Workspace Dependencies

Where feasible, promote shared dependencies to the workspace `Cargo.toml` `[workspace.dependencies]` section so versions are defined once.

### 10.3. No Unused Dependencies

Run `cargo udeps` (or `cargo machete`) periodically. Remove unused crates from `Cargo.toml`.


## 11. Clippy & Formatting

### 11.1. Shared Config

All crates share identical `[lints.clippy]` configuration:

```toml
[lints.clippy]
uninlined_format_args = "warn"
new_without_default = "allow"
```

New crates must copy this block.

### 11.2. `rustfmt.toml`

The workspace root `rustfmt.toml` applies to all crates:

```toml
unstable_features = true
imports_granularity = "Item"
group_imports = "StdExternalCrate"
```

### 11.3. Pre-Commit Check

```bash
./dev.sh all    # format + lint + test
```


## 12. Backlog Items (Non-Blocking)

These inconsistencies exist in the codebase and should be addressed in future refactoring passes. Not required for new code.

| Item | Priority | Effort |
|---|---|---|
| Add `#[cfg(test)] mod tests` to every untested module | Medium | Large |
