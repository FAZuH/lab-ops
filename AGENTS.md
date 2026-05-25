# lab-ops — Agent Instructions

Personal homelab utility tools. Rust workspace, edition **2024**.

## Dev Commands

```bash
./dev.sh all       # format → lint → test
./dev.sh format    # cargo +nightly fmt --all
./dev.sh lint      # cargo clippy --workspace --all-targets --all-features --fix --allow-dirty
./dev.sh test      # cargo test --workspace --all-targets --all-features
```

- **`rustfmt` requires nightly** (`+nightly`). `rustfmt.toml` uses unstable features (`imports_granularity = "Item"`, `group_imports = "StdExternalCrate"`).
- **`.cargo/config.toml` always enables `docker-tests`** via `--cfg feature="docker-tests"`. So `--all-features` in dev commands is redundant but harmless.

## Key Conventions

From `docs/dev/standards.md` (read before adding code):

- **No custom error types** — `color_eyre::Result`, `bail!()`, `wrap_err()`.
- **No `unwrap()`/`expect()`** outside `LazyLock<Regex>` statics.
- **No glob imports** (`use crate::foo::*`), no redundant module paths (`use crate::foo` + `use crate::foo::Bar`).
- **No `process::exit()` in library code** — only `main.rs`.
- **Workspace `run_cli` returns `Result<()>`** and now takes `use_color: bool`.
- **Tracing subscriber initialized ONCE** in root `main.rs`. Workspace crates never init tracing.
- **Root `main.rs` owns the tokio runtime** — workspace crates are `async fn`, no `#[tokio::main]`.
- **Edition 2024** — all crates must set `edition = "2024"`.

## Architecture

| Component | Path | Entrypoint |
|---|---|---|
| Root CLI | `src/` → `main.rs` | `Cli::parse()`, dispatches to commands |
| natmap | `crates/natmap/` → `cli.rs:run_cli()` | iptables NAT daemon + CLI over Unix socket |
| auto-discover | `crates/auto-discover/` → `cli.rs:run_cli()` | Service discovery (Docker + Consul + nginx) |
| lab-lib | `crates/lab-lib/` | Shared types: `TransportProtocol`, `PortAllocator`, Docker helpers |

- **natmap daemon**: central authority for ALL iptables NAT rules. CLI commands talk Unix socket (`/run/natmap.sock`). State in `/var/lib/natmap/state.json`.
- **auto-discover daemon**: runs discovery, forwarding, nginx as concurrent tokio tasks. Component flags: `--no-discovery`, `--no-forwarding`, `--no-nginx`.

## Testing Quirks

- Docker integration tests **require `--test-threads=1`**:
  ```bash
  cargo test --features docker-tests -- --test-threads=1
  ```
  Each test spins up a privileged Ubuntu container with iptables.
- Docker tests are included in `./dev.sh test` (feature always on via `.cargo/config.toml`).
- Run a single crate: `cargo test -p natmap` or `cargo test -p auto-discover`.

## Global CLI Flags

`--verbose` / `-v` (repeatable: info → debug → trace). `--color auto|always|never` (respects `NO_COLOR`). Both are `global = true` — usable after any subcommand.

## Updating Docs

| Changed | Update |
|---|---|
| natmap crate | `docs/natmap/usage.md` |
| auto-discover crate | `docs/auto-discover/usage.md` |
| Root CLI | `docs/lab-ops/usage.md` |
| Code structure | `docs/dev/modules.md` |
| Conventions | `docs/dev/standards.md` |
| Test layout | `docs/dev/testing.md` |
| Architecture | `docs/dev/architecture.md` |
| Integration tests | `docs/dev/integration-test-plan.md` |
| User-facing commands | `README.md` |

Check `docs/dev/standards.md` §12 (backlog) — remove resolved items.
