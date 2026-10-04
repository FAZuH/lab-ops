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
- **`.cargo/config.toml` tries to enable `docker-tests`** via `[build] rustflags`, but that is the *lowest*-precedence rustflags source. Cargo uses exactly one: **`RUSTFLAGS` env > `[target.<triple>]` > `[build]`**. So the cfg is dropped by either of two independent triggers: a user-level `~/.cargo/config.toml` that defines a `[target.<triple>]` table, or any `RUSTFLAGS` that does not itself carry the cfg. Either way every `#[cfg(feature = "docker-tests")]` target compiles to zero tests and reports `ok. 0 passed`, a green result for a suite that never ran. **Pass `--all-features` on any command that must include the Docker suites.** It enables the feature through the feature system rather than rustflags, so it survives every rustflags override. `dev.sh test` and CI already do; a hand-written `cargo test` does not.

### Build environment (NixOS host)

`openssl-sys` cannot find OpenSSL here, so **every** cargo command needs
these exported first, in the same shell as the cargo call:

```bash
export OPENSSL_LIB_DIR=/nix/store/7fr737xfi9qw3fzvdsqmnbqid56knndp-openssl-3.6.4/lib
export OPENSSL_INCLUDE_DIR=/nix/store/0la6k2nj90y1716c1znhdm713ia1qgx8-openssl-3.6.4-dev/include
```

- `OPENSSL_DIR` alone is **not** enough: the `-dev` store path has the headers, the other has the `.so` files, and `openssl-sys` rejects a libdir without them.
- `cargo test -p lab-ops_auto-discover` additionally needs
  `LD_LIBRARY_PATH=/nix/store/7fr737xfi9qw3fzvdsqmnbqid56knndp-openssl-3.6.4/lib`
  or the test binary dies with `libssl.so.3: cannot open shared object file`. The other three crates do not need it.
- Do not "fix" this in `Cargo.toml` — it is the environment, not the code.

## Test Strategy ⚠️

- **Run targeted tests first.** After a change, run the specific test/crate, not the whole suite — it is faster and most changes do not touch the Docker paths. E.g. `cargo test -p lab-ops_natmap`, `cargo test -p lab-ops_auto-discover`.
- **If a test fails, fix and rerun only that test.** Use `cargo test <test_name> -p <crate>`.
- **⚠️ The full suite may be run without asking.** `./dev.sh all` / `./dev.sh test` run the FULL suite including the Docker integration tests (122+ tests, ~2-3 min) — run them before calling work verified when a change touches the Docker harness, the iptables path, the natmap state file, or the auto_discover bootstrap.
- **Nothing enforces `--test-threads=1`.** `.cargo/config.toml` sets only `rustflags`, so the Docker suites run with cargo's default parallelism. Pass `-- --test-threads=1` yourself when you need determinism; they are currently flaky under parallel execution (#54). Each test spins up a privileged Ubuntu container with iptables.

### Quick Test Commands

```bash
cargo test -p lab-ops_natmap                     # natmap unit tests only
cargo test -p lab-ops_auto-discover              # auto-discover unit tests only
cargo test -p lab-ops --all-features --test natmap_docker   # natmap Docker integration tests
cargo test -p lab-ops --all-features --test auto_discover   # auto-discover Docker integration tests
cargo test test_name -p crate_name                 # single test
```

Package names carry the `lab-ops_` prefix (`lab-ops_natmap`,
`lab-ops_auto-discover`). `cargo test -p natmap` fails — the directory is
`crates/natmap/` but the package is not `natmap`.

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
| auto-discover | `crates/auto-discover/` → `cli.rs:run_cli()` | Service discovery (Docker + Consul + forwarding) |
| lab-lib | `crates/lab-lib/` | Shared types: `TransportProtocol`, `PortAllocator`, Docker helpers |

**natmap modules**: `api.rs` (HTTP handlers), `cli.rs` (CLI parsing), `command.rs` (handler functions), `daemon.rs` (state + lifecycle), `iptables.rs` (rule CRUD), `models.rs` (data types), `policy_route.rs` (ip rule/route management).

- **natmap daemon**: central authority for the NAT rules it creates. CLI commands talk Unix socket (`/run/natmap.sock`). State in `/var/lib/natmap/state.json`. `natmap install` creates systemd service. Live rules exposed read-only via `GET /rules`; rule reconciliation stays with each daemon (see `docs/adr/0001-daemon-reports-rules-forwarding-reconciles.md`).
- **auto-discover daemon**: runs discovery and forwarding as concurrent tokio tasks. Component flags: `--no-discovery`, `--no-forwarding`.
- **`policy-route` subcommand** (natmap): manages `ip rule`/`ip route` policy routing for source IP preservation. Used by auto-discover when `preserve_src_ip: true`. API: `POST/DELETE /policy-route`.
- **`DnatConfig.preserve_src_ip`** (renamed from `no_masquerade`; glossary term in `CONTEXT.md`): metadata flag (no iptables change) signaling intent to skip MASQUERADE. Passed through from auto-discover's forwarding sync.

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

## Agent skills

### Issue tracker

Issues and specs live in GitHub Issues (`FAZuH/lab-ops`) — use the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default label vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context — one `CONTEXT.md` + `docs/adr/` at the repo root. See `docs/agents/domain.md`.
