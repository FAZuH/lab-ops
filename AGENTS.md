# OpenCode / Agent Instructions for lab-ops

Personal homelab utility tools, built as a Rust workspace.

## Development Workflow

```bash
./dev.sh all       # format + lint (auto-fix) + test — run before committing
./dev.sh format    # cargo +nightly fmt --all
./dev.sh lint      # cargo clippy --workspace --fix --allow-dirty
./dev.sh test      # cargo test --workspace --all-targets --all-features
./dev.sh docs      # compile Mermaid diagrams
```

## Conventions (Canonical Source)

All codebase conventions are defined in `docs/dev/standards.md`. Read it before adding code. Key constraints:

- **No custom error types** — `color_eyre::Result` everywhere. `bail!()` / `wrap_err()` for errors handlng.
- **No glob imports** — `use crate::models::*` is forbidden. Import each item explicitly.
- **No redundant module imports** — don't `use crate::foo` AND `use crate::foo::Bar`. Pick one.
- **No `process::exit()` in library code** — only `main.rs` may exit the process.
- **Workspace crate `run_cli` returns `Result<()>`** — caller (`main.rs`) handles the exit code.
- **Tracing subscriber is initialized ONCE** in root `main.rs`. Workspace crates never init tracing.
- **Root `main.rs` owns the tokio runtime.** Workspace crates are `async fn`, never `#[tokio::main]`.
- **No `unwrap()` / `expect()`** outside `LazyLock<Regex>` statics. Prefer `?` and `bail!()`.
- **Module-level docs** (`//!`) at the top of every `.rs` file.
- **Public item docs** (`///`) on every `pub` item.
- **Test naming**: `<module_or_function>__<scenario>` (double underscore separator).
- **File naming**: `snake_case.rs`. Root crate's inline subcommands live in `src/cmd/`.
- **No `use clap::Parser as _`** — unused trait imports must be removed.

## Architecture

| Component | Path | Role |
|---|---|---|
| Root binary | `src/` | CLI entrypoint, dispatches subcommands |
| natmap crate | `crates/natmap/` | iptables NAT daemon + CLI (port reservation, Docker mappings) |
| auto-discover crate | `crates/auto-discover/` | Service discovery daemon (Docker events, Consul, nginx configs) |

**natmap daemon** (`lab-ops natmap daemon`) — Central authority for ALL iptables NAT rules. CLI commands (`dnat`, `snat`, `hairpin`, `docker add/rm/remap`, `ls`) communicate via Unix socket (`/run/natmap.sock`). State persisted to `/var/lib/natmap/state.json`.

**auto-discover daemon** (`lab-ops auto-discover daemon`) — Unified daemon running discovery (Docker event watching + natmap + Consul registration), forwarding (kernel DNAT sync from Consul), and nginx (KV config sync) as concurrent tokio tasks. Control components with `--no-discovery`/`--no-forwarding`/`--no-nginx`.

**One-shot commands** (no long-lived daemon): `sync`, `check`, `forwarding-sync`, `nginx-sync`.

## Testing Quirks

- **Docker integration tests** require `--test-threads=1`:
  ```bash
  cargo test --features docker-tests -- --test-threads=1
  ```
  Each test spins up a privileged Ubuntu container with iptables. Parallel execution causes race conditions.
- Docker tests are included in `./dev.sh test` because it uses `--all-features`.
- Run a single crate's tests: `cargo test -p natmap` or `cargo test -p auto-discover`.

## Updating Docs

When editing code, update the relevant docs in the same commit:

| If you change... | Update... |
|---|---|
| auto-discover CLI | `docs/auto-discover/usage.md` |
| Code structure / modules | `docs/dev/modules.md` |
| Conventions / rules | `docs/dev/standards.md` |
| Test layout or counts | `docs/dev/testing.md` |
| Architecture / data flow | `docs/dev/architecture.md` |
| User-facing commands | `README.md` |

Also check `docs/dev/standards.md` §12 (backlog) — if your change resolves a listed item, remove it from the backlog.
