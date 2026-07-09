## Plan: `reconcile_docker_portmaps` — discover containers not in persisted state

### Problem

When natmap is restarted (crash or service restart), containers that started
while it was down are never discovered. `reconcile_docker_portmaps()` at
`daemon.rs:431-486` only processes containers that have entries in the
persisted state (`state.json`). Any container that started during the daemon's
downtime has no state entry and is silently ignored — no iptables rules are
installed, even though the container is running and publishing ports.

### Root Cause

`daemon.rs:479` — after the drain loop, `new_docker` only contains entries from
the old state that survived the reconciliation. There is no scan of running
containers that are *not* in the old state.

```rust
// After the drain loop:
daemon_state.mapping = new_docker;
// new_docker only has containers that were in old state + still running
// No attempt to discover running containers that aren't tracked
```

### Approach

After the existing drain-and-reinstall loop, compute the set of running
containers that aren't tracked in `new_docker`, then discover and install
their port mappings.

### Changes

| # | File | Change |
|---|---|---|
| 1 | `crates/natmap/src/daemon.rs` | Add scanning loop after existing reconciliation in `reconcile_docker_portmaps()` |
| 2 | `crates/natmap/src/daemon.rs` | Add `untracked_container_ids()` helper (pure set diff) |
| 3 | `crates/natmap/src/daemon.rs` | Add unit tests for `untracked_container_ids()` |

### Code Sketch

**New helper** (pure, no Docker/filesystem):

```rust
/// Returns IDs of running containers that have no tracked port mappings.
fn untracked_container_ids<'a>(
    running_ids: &'a HashSet<String>,
    tracked_ids: &'a HashSet<String>,
) -> Vec<&'a str> {
    running_ids
        .iter()
        .filter(|id| !tracked_ids.contains(id.as_str()))
        .map(|id| id.as_str())
        .collect()
}
```

**New scanning loop** (after line 479, before `daemon_state.mapping = new_docker`):

```rust
// Discover untracked containers (started while daemon was down)
if let Some(docker) = &state.docker {
    let tracked: HashSet<String> = new_docker.keys().cloned().collect();
    for id in untracked_container_ids(&running_ids, &tracked) {
        tracing::info!(container.id = %id, "discovering untracked container");
        let Ok(discovered) = docker::get_port_mappings(docker, id).await else {
            continue;
        };
        let mut installed = Vec::new();
        for mut m in discovered {
            m.id = state.allocate_id();
            let host_addr = m.request.host_addr;
            if let Err(e) = ports.allocate(host_addr).await {
                tracing::warn!(host.addr = %host_addr, error = %e,
                    "failed allocating port for untracked container");
                continue;
            }
            if let Err(e) = iptables.install_dockermap(&m) {
                tracing::error!(mapping = ?m, error = %e,
                    "failed to install mapping for untracked container");
                ports.deallocate(host_addr).await;
                continue;
            }
            max_id = max_id.max(m.id);
            installed.push(m);
        }
        if !installed.is_empty() {
            new_docker.insert(id.to_string(), installed);
        }
    }
}
```

The allocation logic parallels `on_container_start()` but skips the
`is_allocated` check — at startup, all ports have been deallocated by
`ports.deallocate_all()` at `daemon.rs:273`, so there can't be conflicts.

### Tests

**Helper unit tests** — `untracked_container_ids()` is pure set difference:

| Test | Given | Expects |
|---|---|---|
| `untracked_returns_empty_when_all_tracked` | `running = {a,b}`, `tracked = {a,b}` | `[]` |
| `untracked_returns_new_ids` | `running = {a,b,c}`, `tracked = {a}` | `[b, c]` (order-independent) |
| `untracked_returns_empty_when_no_running` | `running = {}`, `tracked = {a}` | `[]` |
| `untracked_ignores_tracked_not_running` | `running = {b}`, `tracked = {a,b}` | `[]` — `a` is dead, `b` is tracked |

**Integration test** — the full flow (discover + install iptables rules for
untracked containers) requires Docker API access, which the current test
environment doesn't provide (no Docker socket mounted in the test container).

Options for integration testing (not included in this PR scope):
- Add `-v /var/run/docker.sock:/var/run/docker.sock` to `run_in_docker()` in
  `tests/natmap_docker.rs` to enable Docker-in-Docker
- Then write a test: start daemon with empty state → start a real container
  with published ports → restart daemon → verify iptables rules appear

### Edge Cases

- **Container with no ports**: `docker::get_port_mappings()` returns `Ok(vec![])`,
  the empty result is silently skipped (no entry added to `new_docker`)
- **Container with unresolvable IP**: `get_port_mappings()` returns `Ok(vec![])`
  (see `docker.rs:74-76`), skipped
- **Port allocation failure at startup**: port conflict during startup is
  impossible (`ports.deallocate_all()` ran first), but `allocate()` could fail
  if another process grabbed the port between flush and reconcile

### Verification

```bash
cargo test -p lab-ops_natmap        # unit tests
cargo test -p lab-ops --test natmap_docker  # integration tests (existing)
./dev.sh format && ./dev.sh lint
```
