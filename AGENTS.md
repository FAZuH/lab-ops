# lab-ops — Agent Instructions

Personal homelab utility tools. Rust workspace, edition **2024**.

## Dev Commands

```bash
./dev.sh all       # format → lint → test
./dev.sh format    # cargo +nightly fmt --all
./dev.sh lint      # cargo clippy --workspace --all-targets --all-features --no-deps --fix --allow-dirty
./dev.sh test      # cargo test --workspace --all-targets --all-features
./dev.sh docs      # compile Mermaid .mmd to PNG via mmdc
```

- **`rustfmt` requires nightly** (`+nightly`). `rustfmt.toml` uses unstable features (`imports_granularity = "Item"`, `group_imports = "StdExternalCrate"`).
- **`.cargo/config.toml` always enables `docker-tests`** via `--cfg feature="docker-tests"`. So `--all-features` in dev commands is redundant but harmless.

## Key Conventions

From `docs/dev/standards.md`:

- **No custom error types** — `color_eyre::Result`, `bail!()`, `wrap_err()`.
- **No `unwrap()`/`expect()`** outside `LazyLock<Regex>` statics.
- **No glob imports** (`use crate::foo::*`), no redundant module paths (`use crate::foo` + `use crate::foo::Bar`).
- **No `process::exit()` in library code** — only `main.rs`.
- **Workspace `run_cli`** returns `Result<()>`. `natmap::cli::run_cli(cli, use_color)` takes `use_color: bool`, `auto_discover::cli::run_cli(cli)` does not.
- **Tracing subscriber initialized ONCE** in root `main.rs`. Workspace crates never init tracing.
- **Root `main.rs` owns the tokio runtime** — workspace crates are `async fn`, no `#[tokio::main]`.
- **Edition 2024** — all crates must set `edition = "2024"`.
- **Structured logging** — no string interpolation in log messages. Message is a static label, variable data goes in fields. See `docs/dev/logging.md`.

## Architecture

| Component | Path | Entrypoint |
|---|---|---|
| Root CLI | `src/` → `main.rs` | `Cli::parse()`, dispatches to commands |
| Root cmds | `src/cmd/` | `cf2ansible`, `cf2terra`, `dockernet` |
| natmap | `crates/natmap/` → `cli.rs:run_cli()` | iptables NAT daemon + CLI over Unix socket |
| auto-discover | `crates/auto-discover/` → `cli.rs:run_cli()` | Service discovery (Docker + Consul + nginx) |
| lab-lib | `crates/lab-lib/` | Shared types: `TransportProtocol`, `PortAllocator`, Docker helpers |

- **natmap daemon**: central authority for ALL iptables NAT rules. CLI commands talk Unix socket (`/run/natmap.sock`). State in `/var/lib/natmap/state.json`. `natmap install` creates systemd service.
- **auto-discover daemon**: runs discovery, forwarding, nginx as concurrent tokio tasks. Component flags: `--no-discovery`, `--no-forwarding`, `--no-nginx`.

## Global CLI Flags

`--verbose` / `-v` (repeatable: info → debug → trace). `--color auto|always|never`. Color resolution in `main.rs`: checks `NO_COLOR`, `CLICOLOR` env vars, respects `--color` flag. Both flags are `global = true`.

## Shell Completions

`lab-ops completions bash|zsh|fish|powershell|elvish [--dir <path>]`. When writing to stdout (no `--dir`), zsh output gets `#compdef` stripped and `compdef` appended for safe `eval` use.

## Testing

- Docker integration tests **require `--test-threads=1`**:
  ```bash
  cargo test --features docker-tests -- --test-threads=1
  ```
  Each test spins up a privileged Ubuntu container with iptables.
- Docker tests are included in `./dev.sh test` (feature always on via `.cargo/config.toml`).
- Run a single crate: `cargo test -p natmap` or `cargo test -p auto-discover`.

## Updating Docs

| Changed | Update |
|---|---|
| natmap crate | `docs/natmap/usage.md` |
| auto-discover crate | `docs/auto-discover/usage.md` |
| Root CLI | `docs/lab-ops/usage.md` |
| Code structure | `docs/dev/modules.md` |
| Conventions | `docs/dev/standards.md` |
| Logging standard | `docs/dev/logging.md` |
| Test layout | `docs/dev/testing.md` |
| Architecture | `docs/dev/architecture.md` |
| User-facing commands | `README.md` |

Check `docs/dev/standards.md` §12 (backlog) — remove resolved items.
