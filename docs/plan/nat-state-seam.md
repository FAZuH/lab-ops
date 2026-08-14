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
- [2026-08-14] #28 DONE (superseded by the entry below). Original plan: see "Session state (#28, 2026-08-14)" for the design that shipped.
- [2026-08-14] #28 DONE (implemented, reviewed, fixes applied, tests green). Review fixes: (a) mid-loop allocation-failure leak in `ensure_static_rule` — loop now deallocates all `reserved` ports on ANY failure (allocation error of a later port OR install failure); new regression test `ensure_static_rule_rolls_back_reserved_ports_when_later_port_held` holds the 2nd port at OS level via `std::net::TcpListener::bind` (so `is_allocated` is false but `allocate` fails) and asserts the 1st port is released + re-allocatable; (b) daemon.rs test-module dividers switched `// ── x ──` → `// --- X ---` (daemon.rs ONLY; docker/policy_route/iptables/models test modules keep pre-existing box-drawing style); (c) fixture `tracked_mapping` → `make_tracked_mapping`. Also fixed an edit-corruption where `ensure_static_rule_rolls_back_when_install_fails` accidentally ended up asserting on 39023/39024 instead of 39022. Final: `cargo test -p lab-ops_natmap --lib` = 160 passed (140 existing + 20 new incl. rollback-regression), auto-discover 49, root 65, `cargo check --workspace` + fmt clean. NOT committed.

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

- Goal to re-create: none was active at prepare time. After compaction, continue #28 completion: the implementation + review fixes are DONE and green (160 natmap lib tests, 49 auto-discover, 65 root, workspace check + fmt clean) but NOT committed.
- Next step: hand #28's diff to `review`/`finish` for commit (the implement agent never commits). Then #26 (forwarding sync reconciles against `GET /rules` live rules) and #27 (one `sync_service` primitive in auto-discover) — both unblocked. Delegate each to `/implement` in a fresh context, then `/code-review` before committing.
- Verify with: `cargo test -p lab-ops_natmap --lib` (160), `cargo test -p lab-ops_auto-discover --lib` (49), `cargo test -p lab-ops --lib` (65), `cargo check --workspace`. Do NOT run `./dev.sh all`/`./dev.sh test` (docker-tests) without asking the user.
- Context to re-read first: `docs/plan/nat-state-seam.md` (Decisions & constraints + #25/#28 records below), `CONTEXT.md`, `docs/adr/0001-daemon-reports-rules-forwarding-reconciles.md`, `docs/dev/standards.md` (§4 exception for `NatmapError`; §7.4 `make_` fixture prefix). Key source files now: `crates/natmap/src/iptables.rs` (`Iptables` trait + delegating impl), `crates/natmap/src/daemon.rs` (primitives under `// --- Ensure primitives ---` ~660, `StaticRule` enum, extracted `apply_discovered_mappings`/`reconcile_tracked_mapping`/`ensure_container_mappings`, `Arc<dyn Iptables>` AppState field, FakeIptables + 20 scenario tests), `crates/natmap/src/client.rs` + `utils.rs` (typed seam), `crates/auto-discover/src/daemon.rs` (`ensure_docker_mapping` helper), `crates/auto-discover/src/forwarding.rs` (typed configs + `parse_group_proto`).
- Open questions: none — #25 and #28 designs settled, both shipped and green.

## Session state (#28, 2026-08-14) — IMPLEMENTED + reviewed, fixes applied, tests green

### #28 DONE — implemented, reviewed, fixes applied, tests green (2026-08-14)

### Implement record (design fully settled — this is the brief for resuming)

**Seam: `Iptables` trait** (in `crates/natmap/src/iptables.rs`). `pub trait Iptables` with exactly the 10 methods AppState consumers use: `setup`, `flush_all_natmap`, `install_dockermap`, `remove_mapping`, `install_dnat`, `remove_dnat`, `install_snat`, `remove_snat`, `install_hairpin`, `remove_hairpin` (all `&self`, signatures mirror the current inherent methods). `impl Iptables for IptablesManager` delegates via `IptablesManager::method(self)`. **`AppState.iptables: Arc<IptablesManager>` → `Arc<dyn Iptables>`** (daemon.rs:69). All call sites compile unchanged (trait methods); the 3 test AppState constructors (`create_test_daemon` daemon.rs:741, `test_app_state` api.rs:736, `test_app_state` client.rs:210) coerce via `Arc::new(IptablesManager::new())`. This is the "fake iptables adapter" seam. Ports stay CONCRETE `Arc<PortAllocator>` — real socket binds work on 127.0.0.1 in unit tests (use 39xxx ports; IP_FREEBIND on non-local IPs needs privileges — #25 gotcha).

**Primitives** (free fns in daemon.rs, private, after `impl Daemon`, new section `// --- Ensure primitives ---`):
- `async fn ensure_docker_mapping(ports: &PortAllocator, iptables: &dyn Iptables, mapping: &DockerPortMap) -> Result<()>` — allocate host port (wrap_err "failed to reserve host port") → install_dockermap → on install failure deallocate + return wrapped Err. THE single allocate→install→rollback for docker mappings.
- `enum StaticRule<'a> { Dnat(&'a DnatConfig), Hairpin(&'a HairpinConfig) }` (private).
- `async fn ensure_static_rule(ports: &PortAllocator, iptables: &dyn Iptables, rule: StaticRule<'_>) -> Result<()>` — match &rule for (ext_ip, ports_csv, proto); parse ext_ip → Err on invalid; for each port in CSV (`trim().parse::<u16>()` skip-invalid like should_reconcile's filter_map): skip if `is_allocated`, else allocate + track in `reserved: Vec<SocketAddr>`; install (Dnat→install_dnat, Hairpin→install_hairpin); on install failure deallocate all `reserved` + return wrapped Err. Needs `use std::net::IpAddr; use color_eyre::eyre::WrapErr; use crate::iptables::Iptables;` in daemon.rs; drop `use crate::api::unbind_ports;` (no longer used in daemon.rs).

**New private Daemon methods** (extracted so the 3 scenarios are unit-testable with fakes; exact same logic as current inline code):
- `async fn apply_discovered_mappings(&self, container_id: &str, discovered: Vec<DockerPortMap>) -> Vec<DockerPortMap>` — body of on_container_start's loop: allocate_id → is_allocated? (stale via resolve_stale_container → info! + on_container_stop(stale_id) : warn + continue) → `ensure_docker_mapping(&state.ports, state.iptables.as_ref(), &m)` → on Err warn! "failed to ensure mapping" + continue → assigned.push. `on_container_start` becomes: get_port_mappings → `self.apply_discovered_mappings(...)` → retain/extend/persist (UNCHANGED tail).
- `async fn reconcile_tracked_mapping(&self, container_id: &str, m: DockerPortMap, current_addrs: &HashMap<SocketAddr, SocketAddr>) -> Option<DockerPortMap>` — re-verify container IP via `reconcile_container_addr` (info! "container IP changed on reload") → is_allocated? warn "address already held, removing stale mapping" + None → ensure → Err: warn "failed to ensure mapping, dropping" + None → Some(m).
- `async fn ensure_container_mappings(&self, container_id: &str, discovered: Vec<DockerPortMap>, max_id: &mut u64) -> Vec<DockerPortMap>` — untracked loop: allocate_id → ensure → Err: warn "failed to ensure mapping for untracked container" + continue → max_id.max + installed.push.
- `reconcile_docker_portmaps`: tracked loop `for m in maps { if let Some(kept) = self.reconcile_tracked_mapping(&id, m, &current_addrs).await { max_id=max_id.max(kept.id); kept.push(kept) } }`; untracked loop `let installed = self.ensure_container_mappings(id, discovered, &mut max_id).await;`. Drop now-unused `let ports`/`let iptables` bindings in that fn.
- `reconcile_dnats`/`reconcile_hairpins`: drain loop calls `ensure_static_rule(&self.state.ports, self.state.iptables.as_ref(), StaticRule::Dnat(&config))` / `StaticRule::Hairpin(&config)`; Ok → keep.push, Err → warn "failed to reconcile {dnat,hairpin} rule, dropping". **Delete `should_reconcile`** (daemon.rs:631-659; only used by those two).

**Deliberate scope (report to orchestrator)**: api.rs HTTP handlers (add_mapping/add_dnat/add_hairpin/remap_port) KEEP their inline allocate→install→rollback — they need 409-vs-500 error distinction the `Result<()>` primitive can't express, and the ticket enumerates reload/on_container_start/reconcile only. `reconcile_snats`/`reconcile_policy_routes` unchanged (no port allocation). Behavior changes to note: (a) reconcile tracked loop previously IGNORED install failures and kept the mapping (`let _ =`); now ensure rolls back + drops — matches untracked path; (b) install-failure log downgraded error!→warn! (uniform "failed to ensure ..." — retried next reload); (c) should_reconcile partial-alloc leak fixed (rollback unbinds only what we reserved; pre-held ports untouched).

**Tests** (in daemon.rs `mod tests`): `#[derive(Default)] struct FakeIptables` with `Mutex<Vec<DockerPortMap>> installed_mappings/removed_mappings`, `Mutex<Vec<DnatConfig>> installed_dnats`, `Mutex<Vec<HairpinConfig>> installed_hairpins`, `AtomicBool fail_dockermap/fail_dnat/fail_hairpin` (+ accessor fns); `impl Iptables for FakeIptables` records / returns `Err(eyre!("fake ... failure"))` when flag set. New helper `fn test_daemon_with(state_path, iptables: Arc<dyn Iptables>, ports: Arc<PortAllocator>) -> Daemon`; keep `create_test_daemon` as a thin wrapper (existing 2 tests). Test fns (naming `<module_or_function>_<scenario>`): `ensure_docker_mapping_allocates_and_installs`, `ensure_docker_mapping_rolls_back_when_install_fails`, `ensure_docker_mapping_fails_when_port_held` (stale-guard); `apply_discovered_mappings_stale_deallocates_then_ensures` (stale-deallocate scenario — seed state mapping + pre-allocate port, assert fake removed stale + installed new + port re-reserved), `apply_discovered_mappings_skips_when_port_held_by_active_container`; `ensure_container_mappings_installs_untracked_container` + `ensure_container_mappings_drops_when_install_fails` (untracked-container scenario); `reconcile_tracked_mapping_reinstalls_reverified_ip` (container-IP-reverify scenario — via real `reconcile_container_addr` + ensure, assert fake installed new IP) + `reconcile_tracked_mapping_drops_when_port_held` + `reconcile_tracked_mapping_keeps_stored_ip_when_inspect_missing`; `ensure_static_rule_allocates_and_installs_dnat/hairpin`, `ensure_static_rule_rolls_back_when_install_fails`, `ensure_static_rule_rolls_back_partial_when_later_port_held`, `ensure_static_rule_rejects_invalid_ip`; `reconcile_dnats_drops_config_when_ensure_fails`, `reconcile_dnats_keeps_config_when_ensure_succeeds`, `reconcile_hairpins_routes_through_ensure_static_rule`. Use `make_`-prefixed constructors (`make_addr(port)` → 127.0.0.1, `make_mapping(...)`, `make_dnat(ports_csv)`, `make_hairpin(ports_csv)` with ext_ip "127.0.0.1"). Keep ALL existing tests (resolve_stale_*, untracked_*, reconcile_addr_* are the pre-existing regression tests for the 3 scenarios). Existing unit tests of the 3 bug-fix scenarios are the pure-helper tests in daemon.rs; docker-tests only cover flush-on-reload (not the 3 scenarios).

**Verification**: `cargo test -p lab-ops_natmap --lib`, `cargo test -p lab-ops_auto-discover --lib` (must stay green — do NOT touch auto-discover), `cargo test -p lab-ops --lib`, `cargo check --workspace`, `cargo +nightly fmt --all`. Delegate suite runs to `test` subagent. Do NOT run docker-tests / `./dev.sh all|test`. Do NOT commit — `finish` subagent owns commits.
- Pre-existing dirty files NOT from the #25 session (do not sweep into the #25 commit): `Cargo.toml`, `Cargo.lock`, `tests/auto_discover/*` (incl. untracked `startup_race.rs`), and the `should_sweep_stale`/`sync_errors` hunks in `crates/auto-discover/src/daemon.rs` (previous session's work, interleaved with #25's client-swap hunks — split at line level with `git add -p`). Session docs: `CONTEXT.md`, `docs/adr/`, `docs/agents/`, `AGENTS.md` (edited), `docs/plan/nat-state-seam.md`, `docs/dev/modules.md` (edited), `docs/dev/standards.md` (edited).
