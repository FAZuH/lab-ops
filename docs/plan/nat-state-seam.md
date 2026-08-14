# Plan: NAT-state seam cluster (tickets #25–#28)

## Objective

Implement the architecture-review cluster "one typed seam through which all NAT state flows", per spec #24. Four tickets on GitHub (`FAZuH/lab-ops`):

- **#25** Typed natmap client + honest vocabulary — no blockers
- **#28** One ensure primitive per rule kind in the natmap daemon — no blockers
- **#26** Forwarding sync reconciles against daemon-reported live rules — blocked by #25
- **#27** One `sync_service` primitive in auto-discover — blocked by #25

Each ticket is agent-ready (`ready-for-agent` label, acceptance criteria in body). Work the frontier: #25 and #28 first, then #26/#27 once #25 lands.

Deferred from the same architecture review (future specs): single port authority (candidate 4) and container inspection into lab-lib (candidate 5) — their attachment point is the internal port-decision seam #27 introduces.

## Decisions & constraints

Settled in the grilling session (all user-approved):

1. **Daemon reports live rules; forwarding reconciles** — new `GET /rules` returns structured live rules; reconciliation stays with each daemon. Recorded in `docs/adr/0001-daemon-reports-rules-forwarding-reconciles.md`.
2. **Collapse within each crate** — no cross-crate shared transaction primitive. Auto-discover gets one `sync_service`; natmap gets `ensure_docker_mapping`/`ensure_static_rule`.
3. **Typed client in the natmap crate** (public `client` module) calling `request_json` directly — removes the `Cli`/`run_cli` round-trip.
4. **`DnatConfig.no_masquerade` renamed to `preserve_src_ip`** across the seam; remains metadata (no iptables change). `preserve_src_ip` is a glossary term in `CONTEXT.md`.
5. **Typed error enum** in natmap; auto-discover matches variants, not `"409"`/`"404"` strings.
6. **`GET /rules` returns structured `LiveRule { kind, ext_ip, int_ip, ports, proto }`**, parsed from the daemon's `get_rules` output, comment-attributed, multiport-capable — fixes the stale-multiport gap (forwarding's `parse_dnat_rule` can't match `--dports`).
7. **`sync_service(target, resolved)`** — one primitive for both sync and event paths; port-decision behind an internal seam.
8. **Orchestrators stay** in natmap (reload/on_container_start/reconcile differ in input set); the per-rule apply step is shared.

Constraints from `AGENTS.md`: unit tests only (no `./dev.sh test` / docker-tests without the user), `--test-threads=1` is enforced by `.cargo/config.toml`, nightly rustfmt, no custom error types (`color_eyre`), no `unwrap()` outside `LazyLock<Regex>`, structured tracing.

## File layout

- `crates/natmap/src/client.rs` (new) — typed client module over `request_json`.
- `crates/natmap/src/utils.rs` — `request_json` gains a typed error path (status → variant).
- `crates/natmap/src/models.rs` — `DnatConfig.no_masquerade` → `preserve_src_ip`; `LiveRule` model.
- `crates/natmap/src/api.rs` — `GET /rules` handler; typed error responses.
- `crates/natmap/src/daemon.rs` — `ensure_docker_mapping`/`ensure_static_rule`; reload/on_container_start/reconcile route through them.
- `crates/auto-discover/src/natmap.rs` — replaced by the client; string-matching removed.
- `crates/auto-discover/src/forwarding.rs` — reconciles against reported rules; `find_stale_rules`/`parse_dnat_rule` deleted; error handling fixed.
- `crates/auto-discover/src/daemon.rs` — `sync_service(target, resolved)`; sync/event paths unified.
- `CONTEXT.md` — already written (glossary). Keep in sync as terms settle.

## Design

Single seam: the typed client interface. Everything auto-discover does to NAT state crosses it; everything the daemon reports crosses it back. `GET /rules` (live rules) is distinct from `GET /mappings` (persisted state). The daemon keeps flush-and-reinstall on reload — do NOT change that. Multiport parsing is required on the daemon side (`-m multiport --dports a,b`), attributed by comment (`natmap:dnat:`, `natmap:hairpin:`, `natmap:<container>:<port>`).

Test seams (user-approved): the typed client against the daemon's in-process axum router (fake iptables/ports adapters); forwarding reconcile and `sync_service` against in-memory adapters. No privileged containers.

## Execution

- Delegate each ticket to `/implement` in a fresh context window, per ask-matt flow. Ticket body + this doc are the brief.
- Verify per ticket: `cargo test -p natmap`, `cargo test -p auto-discover`, `cargo test -p lab-ops --no-default-features` unit scopes only; clippy via `./dev.sh lint`; do NOT run `./dev.sh all`/`./dev.sh test` (docker-tests) without asking the user.
- After each ticket, `/code-review` the diff before committing (via `finish`/orchestrator).

## Deviation log

- (none yet)
- [2026-08-14] #25 DONE (implemented, reviewed, fixes applied). Deviations from plan recorded by the implement agent:
  - 409-conflict client test pre-allocates `127.0.0.1:8080` on the test PortAllocator instead of a fake public IP — `IP_FREEBIND` to a non-local address needs privileges the test env lacks; loopback still exercises the same `bind_ports`→EADDRINUSE→409 path.
  - Review (fix-first) found and fixed: `group.proto.parse().unwrap_or_default()` in forwarding.rs silently downgraded invalid protocols to Tcp where the daemon previously rejected them → replaced with `parse_group_proto(&str) -> Result<TransportProtocol>` gated with `?` (sync now fails on invalid proto, matching old behavior) + 3 new tests. Section delimiters in client.rs fixed to `// --- Name ---` standard. Test names `inspect_output_*` → `parse_docker_inspect_output_*`.
  - Pending at commit time: `crates/natmap/src/client.rs` is untracked — MUST be staged with the #25 commit or the workspace breaks.
- [2026-08-14] #25 verification: `cargo test -p lab-ops_natmap --lib` 140 pass, `-p lab-ops_auto-discover --lib` 49 pass, root `--lib` 65 pass, `cargo check --workspace` clean, fmt applied. No other tickets started.

## Session state (2026-08-14, #25)

### #25 DONE — implemented, reviewed, fixes applied, tests green (2026-08-14)

Verification: `cargo test -p lab-ops_natmap --lib` 140 pass, `-p lab-ops_auto-discover --lib` 49 pass, root `--lib` 65 pass, `cargo check --workspace` clean, `cargo +nightly fmt --all` applied. Acceptance greps clean (no `"409"`/`"404"`/`"Container not found"` string-matches in auto-discover; no `run_cli`/`cli::Cli`/`cli::Docker` in auto-discover). CLI behavior unchanged (`--no-masquerade` flag preserved via `#[arg(long = "no-masquerade")]`).

### Implement record (kept from the implement agent, for reference)

Code written so far (compiles, tests run against `cargo test -p lab-ops_natmap --lib`):

- **models.rs** — `no_masquerade` → `preserve_src_ip` in `DnatConfig` + `DnatRequest` (+ test literals). Added `From<DnatConfig> for DnatRequest`, `From<SnatConfig> for SnatRequest`, `From<HairpinConfig> for HairpinRequest`, `From<PolicyRouteConfig> for PolicyRouteRequest` (config → request conversion for the client).
- **utils.rs** — `NatmapError` enum added (variants: `Connect(String)`, `Http(String)`, `Json(String)`, `BadRequest(String)`, `NotFound(String)`, `Conflict(String)`, `Internal(String)`, `Unavailable(String)`, `UnexpectedStatus { status: u16, body: String }`), hand-implemented `Display` + `std::error::Error` (no thiserror). `request_json` now returns `Result<T, NatmapError>`; status→variant via `from_status()`. Empty-body success → `from_value(Value::Null)` (for `()` responses). Tests: canned hyper server over temp Unix socket → status→variant mapping (400/404/409/500/503/418), error body carried in variant, Connect error. **Note: CLI `command.rs` still compiles via `?` (eyre From<E>)**.
- **client.rs** (new) — `pub struct NatmapClient { socket: PathBuf }`, `#[derive(Debug, Clone)]`, `new(impl Into<PathBuf>)`, `default_socket()` (env `NATMAP_SOCKET` else `lab_ops_lab_lib::NATMAP_SOCKET`). Methods: `dnat(DnatConfig, bool) -> Result<Option<DnatConfig>>`, `snat`, `hairpin`, `policy_route` (all same shape, delete → `Ok(None)`), `add_mapping(&str, DockerAddMapRequest) -> Result<DockerPortMap>`, `remove_mapping(&str, u16)`, `remove_mapping_by_id(u64)`, `remap_port(&str, DockerRemapRequest) -> Result<Vec<DockerPortMap>>`, `list_mappings() -> Result<ListResponse>`, `clear()`. Uses `crate::utils::request_json` directly + `From` impls. Tests: real axum router over temp Unix socket (`spawn_daemon` helper: `UnixListener` + `TokioIo` + `Builder::new(TokioExecutor)` + `service_fn(|req| app.clone().call(req))`), covering dnat delete-not-found OK, dnat invalid-ports→BadRequest, dnat port-pre-allocated→Conflict, snat not-found→NotFound, add_mapping no-docker→Unavailable, remove_mapping/by-id/remap→NotFound, list roundtrip, clear OK, policy_route add echo/delete OK.
- **daemon.rs** — extracted `pub fn build_router(state: AppState) -> Router` (all 13 routes) from `Daemon::new`; `Daemon::new` now `Ok(Self { app: build_router(state.clone()), state })`.
- **api.rs** — `preserve_src_ip` rename in `add_dnat`/`remove_dnat` config building + 4 test literals.
- **iptables.rs** — 6 test literals renamed `preserve_src_ip: false`.
- **command.rs** — `handle_dnat` param renamed `preserve_src_ip` (CLI path, still `Result<()>` via eyre).
- **cli.rs** — field renamed `preserve_src_ip` but **flag stays `--no-masquerade`** via `#[arg(long = "no-masquerade")]` (CLI behavior unchanged); run_cli destructure updated.
- **lib.rs** — added `pub mod client;`.

`cargo test -p lab-ops_natmap --lib` — re-run after the BoxBody fix: green (140 pass).

### Remaining work for #25 (in order) — completed; kept as the implementation record

1. Re-run `cargo test -p lab-ops_natmap --lib` (expect green after the BoxBody fix).
2. **auto-discover**: delete `crates/auto-discover/src/natmap.rs` entirely. Move `get_container_ip` + `parse_docker_inspect_output` (+ tests) to `crates/auto-discover/src/docker.rs` as free functions. Wire `daemon.rs` + `forwarding.rs` to `lab_ops_natmap::client::NatmapClient`:
   - daemon.rs: `use lab_ops_natmap::client::NatmapClient;` (struct field + `default_socket()` unchanged shape); imports for `lab_ops_natmap::client::NatmapError`, `lab_ops_natmap::models::{DockerAddMapRequest, PolicyRouteConfig}`. Add `DiscoveryDaemon::ensure_docker_mapping(&self, container_id, bind_ip: Option<&str>, host_port, container_port, proto, target_ip: Option<&str>) -> Result<()>` helper: builds `DockerAddMapRequest { host_ip: bind_ip.unwrap_or("0.0.0.0"), host_port, container_port, target_ip, proto }`, calls `natmap.add_mapping`, swallows `NatmapError::Conflict`/`NotFound` with structured `tracing::warn!`, else `wrap_err`. Replace the three `self.natmap.add_docker_mapping(...)` call sites (sync_docker, sync_local) and the two `self.natmap.policy_route(...)` call sites + the one in `handle_container_die` (build `PolicyRouteConfig { src_ip, via, table: 100 }`, pass delete bool). `determine_consul_ip` calls `crate::docker::get_container_ip(container_id)` instead of `self.natmap.get_container_ip`. Preserve the pre-existing `should_sweep_stale`/`sync_errors` work.
   - forwarding.rs: swap `crate::natmap::NatmapClient` → `lab_ops_natmap::client::NatmapClient`; build `DnatConfig { ext_ip, int_ip, ports: ports_csv, proto: group.proto.parse().unwrap_or_default(), ext_if: None, preserve_src_ip: group.preserve_src_ip }` and `HairpinConfig { ext_ip, int_ip, ports, proto, lan_cidr }` per call site; `natmap.dnat(config, delete)` / `natmap.hairpin(config, delete)`.
   - daemon.rs `NatmapClient` is used by both — keep `.wrap_err(...)` calls (eyre From NatmapError works).
3. Verify: `cargo test -p lab-ops_auto-discover --lib`, `cargo check --workspace`, `cargo test -p lab-ops --lib` (root crate unit-only — NOT the docker-tests, they're always-on via `.cargo/config.toml` rustflags). Do NOT run `./dev.sh all`/`test`. rustfmt nightly at end.
4. Acceptance grep: no `"409"`/`"404"`/`"Container not found"` string-matches in auto-discover; no `run_cli`/`cli::Cli`/`cli::Docker` in auto-discover.

### Design decisions (kept from earlier session — see bottom of this doc)

Error enum in utils.rs (hand-written, no thiserror — Cargo.toml dirty); client API surface above; rename keeps CLI flag via `#[arg(long = "no-masquerade")]`; auto-discover natmap.rs deleted, 409/404 swallow → variant match in a `DiscoveryDaemon` helper; client tests hit real router over temp socket; docker-tests always-on gotcha (unit-only = `--lib`).

1. **Error enum**: `NatmapError` lives in `crates/natmap/src/utils.rs` (with `request_json`,
   which gains the typed error path per plan file layout). Variants: `Connect(String)`,
   `Http(String)`, `Json(String)`, `BadRequest(String)` (400), `NotFound(String)` (404),
   `Conflict(String)` (409), `Internal(String)` (500), `Unavailable(String)` (503),
   `UnexpectedStatus { status: u16, body: String }`. Hand-implemented `Display` +
   `std::error::Error` — do NOT add `thiserror` (would touch dirty `Cargo.toml`/`Cargo.lock`).
   `request_json` returns `Result<T, NatmapError>`; natmap `command.rs` (CLI) keeps working
   via `?` (color_eyre `Report: From<E: StdError>`) and `Err(_)` matches.
2. **Client API** (`crates/natmap/src/client.rs`, `pub struct NatmapClient { socket: PathBuf }`):
   one typed method per daemon op. Static ops take the typed config struct + `delete: bool`
   and return `Result<Option<T>, NatmapError>` (Some = daemon echo on add, None = delete):
   `dnat(DnatConfig, bool)`, `snat(SnatConfig, bool)`, `hairpin(HairpinConfig, bool)`,
   `policy_route(PolicyRouteConfig, bool)`. Docker ops (separate add/remove, matching the
   CLI which has `docker add/rm/remap` subcommands rather than a `--delete` flag):
   `add_mapping(&str, DockerAddMapRequest) -> Result<DockerPortMap>`,
   `remove_mapping(&str, u16)`, `remove_mapping_by_id(u64)`, `remap_port(&str, DockerRemapRequest)`,
   `list_mappings() -> ListResponse`, `clear()`. Plus `new(impl Into<PathBuf>)` and
   `default_socket()` (env `NATMAP_SOCKET` else `lab_ops_lab_lib::NATMAP_SOCKET`).
   Config→Request conversion happens inside the client (`DnatConfig`→`DnatRequest`, etc.).
   `pub use crate::utils::NatmapError;` re-exported from client.
3. **Rename**: `no_masquerade` → `preserve_src_ip` in `DnatConfig` + `DnatRequest`
   (models.rs), api.rs (2 spots + 6 test spots), iptables.rs (6 test spots), command.rs,
   cli.rs. **CLI flag stays `--no-masquerade`** (use `#[arg(long = "no-masquerade")]` on a
   field named `preserve_src_ip`) to keep CLI behavior unchanged per #25 acceptance #6.
   Metadata-only — nothing consumes it in iptables rule building (verified: iptables.rs
   build_* never reads it).
4. **auto-discover**: delete `crates/auto-discover/src/natmap.rs` entirely. `daemon.rs` +
   `forwarding.rs` import `lab_ops_natmap::client::NatmapClient` directly. The 409/404
   swallow moves to a `DiscoveryDaemon` helper in daemon.rs, matching
   `NatmapError::Conflict`/`NotFound` variants (not strings), structured warn fields.
   `get_container_ip` + `parse_docker_inspect_output` (+ its tests) move to
   `crates/auto-discover/src/docker.rs`; `build_mapping_spec` (+ tests) is deleted.
   Call sites build `DockerAddMapRequest { host_ip: bind_ip.unwrap_or("0.0.0.0"), host_port,
   container_port, target_ip, proto }` and `PolicyRouteConfig { src_ip, via, table }`.
5. **Tests**: natmap client tests hit the REAL axum router over a temp Unix socket —
   extract `fn build_router(state: AppState) -> Router` from `Daemon::new` (used by both),
   test helper binds Unix listener + `TokioIo` + `Builder::new(TokioExecutor)` serving
   `service_fn(|req| app.clone().call(req))`. Tests: `dnat_delete_returns_ok_when_not_found`
   (DELETE 200), `dnat_add_invalid_ports_maps_bad_request` (400), 409 via pre-allocating the
   port with `PortAllocator::allocate` then POST (bind_ports → Conflict, no iptables needed),
   `remove_mapping`/`remove_mapping_by_id`/`remap` 404s, `list_mappings` roundtrip,
   `clear` OK. Plus utils.rs canned-server unit tests for status→variant mapping
   (400/404/409/500/503/unknown) without iptables. **No iptables-dependent success paths**
   (POST /dnat success needs real iptables → skip).
6. **Docker-test gating gotcha**: `.cargo/config.toml` rustflags set
   `--cfg feature="docker-tests"` ALWAYS, so `cargo test -p <crate>` compiles+runs docker
   integration tests. Unit-only scope = `cargo test -p lab-ops_natmap --lib`,
   `cargo test -p lab-ops_auto-discover --lib`, `cargo test -p lab-ops --lib`.
   Also `tests/auto_discover/mod.rs` is `#![cfg(feature = "docker-tests")]` (dirty file, leave alone).
7. **Pre-existing dirty files to NOT touch**: root `Cargo.toml` (has `[[test]] auto_discover`),
   `Cargo.lock`, `tests/auto_discover/recovery.rs`, `tests/auto_discover/mod.rs`,
   `tests/auto_discover/startup_race.rs`, `crates/auto-discover/src/daemon.rs` (has uncommitted
   `should_sweep_stale` work from another session — I WILL still edit daemon.rs for the client
   swap, preserving that work), `AGENTS.md`, `CONTEXT.md`/`docs/` (uncommitted docs).
8. **Verification commands** (after each increment): `cargo test -p lab-ops_natmap --lib`,
   `cargo test -p lab-ops_auto-discover --lib`, then root `cargo test -p lab-ops --lib`;
   `cargo check --workspace` for full build. Do NOT run `./dev.sh all|test`. Delegate test runs
   to the `test` subagent. rustfmt is nightly (`cargo +nightly fmt --all`) — run at end.
9. **Acceptance checklist for #25**: (a) public client module in natmap, auto-discover uses it,
   no `Cli`/`run_cli` in auto-discover's client path; (b) typed config structs + delete flag,
   `preserve_src_ip` rename across workspace; (c) typed error enum, auto-discover matches
   variants; (d) no string-match on "409"/"404"/"Container not found" left in auto-discover;
   (e) workspace builds + unit tests pass; (f) natmap + auto-discover CLI behavior unchanged
   (flags, subcommands, outputs).

## Resume checkpoint

- Goal to re-create: none was active at prepare time. After compaction, create a goal for ticket #28, e.g. "Implement #28 — one ensure primitive per rule kind in the natmap daemon (per spec #24), unit tests passing, no docker-tests."
- Next step: #28 (no blockers) — `ensure_docker_mapping`/`ensure_static_rule` primitives in the natmap daemon with reload/on_container_start/reconcile routing through them. Then #26 (forwarding sync reconciles against `GET /rules` live rules) and #27 (one `sync_service` primitive in auto-discover) — both now unblocked by #25. Delegate each ticket to `/implement` in a fresh context, then `/code-review` before committing.
- Verify with: `cargo test -p lab-ops_natmap --lib`, `cargo test -p lab-ops_auto-discover --lib`, `cargo test -p lab-ops --lib`, `cargo check --workspace`. Do NOT run `./dev.sh all`/`./dev.sh test` (docker-tests) without asking the user.
- Context to re-read first: `docs/plan/nat-state-seam.md` (Decisions & constraints + #25 record above), `CONTEXT.md`, `docs/adr/0001-daemon-reports-rules-forwarding-reconciles.md`, `docs/dev/standards.md` (§4 exception for `NatmapError`). Key source files now: `crates/natmap/src/client.rs` + `utils.rs` (typed seam), `crates/natmap/src/daemon.rs` (`build_router` extracted), `crates/auto-discover/src/daemon.rs` (`ensure_docker_mapping` helper), `crates/auto-discover/src/forwarding.rs` (typed configs + `parse_group_proto`).
- Open questions: none — #25 design is settled and shipped.
- Pre-existing dirty files NOT from the #25 session (do not sweep into the #25 commit): `Cargo.toml`, `Cargo.lock`, `tests/auto_discover/*` (incl. untracked `startup_race.rs`), and the `should_sweep_stale`/`sync_errors` hunks in `crates/auto-discover/src/daemon.rs` (previous session's work, interleaved with #25's client-swap hunks — split at line level with `git add -p`). Session docs: `CONTEXT.md`, `docs/adr/`, `docs/agents/`, `AGENTS.md` (edited), `docs/plan/nat-state-seam.md`, `docs/dev/modules.md` (edited), `docs/dev/standards.md` (edited).
