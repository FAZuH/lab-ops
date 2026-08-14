# Spec: Single port authority through the natmap daemon

## Problem Statement

Port assignment is split-brain. Auto-discover owns a per-pass `PortAssignments` file that it loads and saves independently, so two concurrent paths (`sync` and the container-event loop) can run with two live instances and both write `ports.json`. Its `decide_ports` step guesses availability with a local `is_port_free` OS bind-check on the natmap bind IP, while the natmap daemon separately reserves ports with socket pre-binding and arbitrates conflicts with HTTP 409. Two different "is this port taken" opinions exist in two crates, and the only cross-check is the 409 path. The result is duplicated conflict logic, a heuristic that can disagree with the real authority, and no single place that owns the answer to "may I use this port?".

## Solution

Make the natmap daemon the single authority for both NAT rules and port assignment. Auto-discover stops owning port allocation and stops bind-checking locally; it asks the daemon for a mapping and lets the daemon's allocate->install->rollback (with 409 arbitration) answer whether a port is available. The local `is_port_free` pre-filter and the per-pass `PortAssignments` ownership are removed from auto-discover. The local ForwardRemote behavior "port taken -> skip the mapping but still register the service in Consul" is preserved, now driven by the daemon's 409 conflict rather than a local bind-check.

## User Stories

1. As the auto-discover daemon, I want one authority to answer "may I map this host port?", so that my local bind-check can't disagree with the real allocator.
2. As the auto-discover daemon, I want to stop loading and saving `ports.json`, so that two concurrent sync paths can no longer race on a shared file.
3. As the auto-discover daemon, I want the local ForwardRemote "port taken -> skip mapping but still register" behavior preserved, so that a service is registered in Consul even when its port is unavailable.
4. As the natmap daemon, I want to hand out host ports and arbitrate conflicts over the existing HTTP seam, so that allocation lives next to the rules that consume it.
5. As a test, I want `decide_ports` / `sync_service` driven against an in-memory natmap adapter that reports conflicts, so that the skip-but-register behavior is unit-tested without a real daemon.
6. As an operator, I want NAT and port-assignment behavior unchanged, so that the refactor is invisible from the CLI and Consul.

## Implementation Decisions

- **natmap daemon is the port authority.** The existing `PortAllocator` (socket pre-bind reservation) remains the single allocator, owned by the daemon. Port availability is answered where the rules are built.
- **Allocation rides on the mapping flow (allocate-when-needed).** No separate allocate endpoint. When auto-discover requests a mapping for a host port that is already taken, the daemon returns 409, exactly as today; when a free port must be chosen (the RProxy / unbound ForwardLocal cases), the daemon's allocate-when-needed step supplies it. This keeps the client surface small and the 409 arbitration exactly where it already is.
- **Auto-discover drops local ownership.** The per-pass `PortAssignments` load/save and the `is_port_free` bind-check in `decide_ports` are removed. `decide_ports` becomes a pure translation of desired intent into a mapping request; all availability answers come from the daemon.
- **Skip-but-register preserved via 409.** The existing non-fatal 409 handling in the mapping path is the arbitration. The local ForwardRemote distinction (skip mapping but still register) is expressed as: on 409 conflict for a local target, do not fail the service — register it in Consul without the mapping.
- **Port decisions ride the existing `NatmapOps` seam** (the adapter trait introduced in #27), so `sync_service` and `decide_ports` stay testable with an in-memory fake that reports conflicts.

## Testing Decisions

- A good test exercises the authority boundary: the daemon arbitrates a conflict, and auto-discover reacts to the arbitration — not the internals of either allocator.
- **natmap**: the mapping flow against the in-process router with a fake iptables adapter, asserting 409 on a taken port and a successful allocate when none is requested.
- **auto-discover**: `decide_ports`/`sync_service` against an in-memory `NatmapOps` fake that can be configured to report a conflict, asserting the service is still registered (skip-but-register) and that no local bind-check happens.
- Prior art: the in-memory natmap/Consul adapters in `crates/auto-discover/src/daemon.rs` tests, and the api.rs handler tests over the in-process router.

## Out of Scope

- Moving rule reconciliation into the natmap daemon — rejected, ADR-0001.
- A separate pre-allocation/reservation endpoint or lease mechanism — allocation stays tied to the mapping flow.
- Changing the daemon's flush-and-reinstall reconcile strategy.
- Candidate 5 (container inspection into lab-lib) — separate spec.

## Further Notes

- ADR-0001 records the daemon-as-central-authority principle; this spec extends it from rule ownership to port ownership.
- The port-decision seam `decide_ports` in `crates/auto-discover/src/daemon.rs` is the stated attachment point (comment on the seam names this spec).
- CONTEXT.md vocabulary: port mapping, NAT rule, live rule, reconcile, natmap daemon.

---

## Execution record (ticket #31 — 2026-08-15, worktree lab-ops-c4, branch feat/c4-port-authority)

**DONE. Natmap-side piece shipped; auto-discover follow-up is a separate ticket.**

Implemented (natmap crate only):
- `host_port: 0` sentinel = "daemon allocates" (NOT `Option<u16>` — auto-discover in this worktree constructs `DockerAddMapRequest` with a `u16` at `crates/auto-discover/src/daemon.rs:625` and must not be touched; port 0 was already a dead value).
- `api.rs`: `allocate_free_port()` scans `EPHEMERAL_PORT_START..=EPHEMERAL_PORT_END` (pub(crate) consts 32768..=61000; lab-lib's range is private) via direct `ports.allocate()` so the bind itself arbitrates races; returns 503 `SERVICE_UNAVAILABLE` on range exhaustion. Handler allocates at the front instead of a second bind; 409 path unchanged; install-failure `deallocate` runs for both paths.
- `models.rs`: `host_port` doc — "0 asks the daemon to allocate a free port from its ephemeral range and report the chosen port".
- `daemon.rs`: `FakeIptables::installed_mappings` + `set_fail_dockermap` made `pub(crate)` (used by client.rs/api.rs tests).
- 4 new tests: api no-host-port allocates (127.0.0.2), api taken-port 409 (127.0.0.1:39040), api install-failure releases (127.0.0.4), client round-trip (127.0.0.3). `add_mapping_with_target_ip_success` adapted `host_port: 0` → `39050`.

**Flake found & solved (important — environmental, not a test race):** the exact-port assertion `re-picks 32771` raced the OS, not other tests. This box's `ip_local_port_range = 32768-60999` overlaps the scan range; the OS hands out ephemeral source ports from the bottom (32768, 32769…) for outbound conns, so port 32771 is transiently held ~25–40% of the time. A wildcard `0.0.0.0:port` bind blocks any specific-loopback bind on that port (proved empirically), so loopback-IP isolation (127.0.0.2/.3/.4) cannot help. **Fix:** structural assertion — after the failed install, scan the allocator MAP (which external OS traffic never touches) `32768..=32780` for leaks instead of asserting an exact port.

Verification (all green):
- single test 1/1; `--test-threads=1` 7/7; parallel `add_mapping` filter 10/10 ×2 (pre+post fmt); full natmap lib 175 ×3; root `lab-ops --lib` 65; `lab-ops_auto-discover --lib` 68 (untouched); `cargo check --workspace` clean; `cargo +nightly fmt --all` applied.
- clippy: only pre-existing warnings — `field_reassign_with_default` (daemon.rs:1666-1707) and `set_fail_hairpin` dead code, both present at base commit bf9e16a.
- Uncommitted (per user rule: never push/commit directly; orchestrator/finish handles it). Working tree: 4 modified files (api.rs, client.rs, daemon.rs, models.rs).

Deviations from spec:
- Spec's user story 3/4 ("auto-discover drops local ownership", skip-but-register via 409) is the auto-discover side — NOT in this ticket's scope; #31 is natmap-only.
- GOTCHA: `~/.cargo/config.toml` sets a GLOBAL `target-dir = /home/fazuh/.cache/cargo-target` shared by all three worktrees (lab-ops, -c4, -c5). Another worktree rebuilding `lab_ops_natmap` left a stale 171-test binary that cargo reused (the 4 new tests appeared "missing"); fix = `touch` the crate sources to invalidate the fingerprint and rebuild. Papercut pc_244ef8a6ef95.

## Resume checkpoint
- Goal to re-create: none (work complete; nothing outstanding).
- Next step: report completion to the orchestrator — ticket #31 is implemented, flake root-caused and fixed, full verification green. Then hand the diff for `/review` (code-review skill) before `finish` commits.
- Verify with: `cargo test -p lab-ops_natmap --lib` (expect 175), `cargo test -p lab-ops --lib` (65), `cargo check --workspace`, `cargo +nightly fmt --all`.
- Context to re-read first: this file's Execution record + the `#31 port-authority` project memory block; code at `crates/natmap/src/api.rs` (`allocate_free_port`, `add_mapping` handler, tests module).
- Open questions: none blocking. Auto-discover side (drop `PortAssignments`/`is_port_free`, skip-but-register via 409) is a separate future ticket.
