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

## Test Strategy ⚠️

- **Run ONLY relevant tests first.** After a change, run the specific test/crate, not the full suite. E.g. `cargo test -p natmap`, `cargo test -p auto-discover`.
- **If a test fails, fix and rerun only that test.** Use `cargo test <test_name> -p <crate>`.
- **⚠️ `./dev.sh all` runs the FULL suite including Docker integration tests (122+ tests, ~2-3 min).** Do NOT run `./dev.sh all` or `./dev.sh test` without asking the user first. Notify the user and let them trigger it themselves.
- **Docker tests require `--test-threads=1`** (enforced by `.cargo/config.toml`). Each test spins up a privileged Ubuntu container with iptables.

### Quick Test Commands

```bash
cargo test -p natmap                              # natmap unit tests only
cargo test -p auto-discover                       # auto-discover unit tests only
cargo test -p lab-ops --test natmap_docker         # natmap Docker integration tests
cargo test -p lab-ops --test auto_discover         # auto-discover Docker integration tests
cargo test test_name -p crate_name                 # single test
```

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

**natmap modules**: `api.rs` (HTTP handlers), `cli.rs` (CLI parsing), `command.rs` (handler functions), `daemon.rs` (state + lifecycle), `iptables.rs` (rule CRUD), `models.rs` (data types), `policy_route.rs` (ip rule/route management).

- **natmap daemon**: central authority for ALL iptables NAT rules. CLI commands talk Unix socket (`/run/natmap.sock`). State in `/var/lib/natmap/state.json`. `natmap install` creates systemd service.
- **auto-discover daemon**: runs discovery, forwarding, nginx as concurrent tokio tasks. Component flags: `--no-discovery`, `--no-forwarding`, `--no-nginx`.
- **`policy-route` subcommand** (natmap): manages `ip rule`/`ip route` policy routing for source IP preservation. Used by auto-discover when `preserve_src_ip: true`. API: `POST/DELETE /policy-route`.
- **`DnatConfig.no_masquerade`**: metadata flag (no iptables change) signaling intent to skip MASQUERADE. Passed through from auto-discover's forwarding sync.

## Global CLI Flags

`--verbose` / `-v` (repeatable: info → debug → trace). `--color auto|always|never`. Color resolution in `main.rs`: checks `NO_COLOR`, `CLICOLOR` env vars, respects `--color` flag. Both flags are `global = true`.

## Shell Completions

`lab-ops completions bash|zsh|fish|powershell|elvish [--dir <path>]`. When writing to stdout (no `--dir`), zsh output gets `#compdef` stripped and `compdef` appended for safe `eval` use.

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
