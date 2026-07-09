## Plan: Fix race in `on_container_start` — deallocate stale mappings

### Problem

When a Docker container is recreated (`docker compose down && up`), the `start`
event for the **new** container arrives before the `die` event for the **old**
one. `on_container_start()` at `daemon.rs:395` checks `is_allocated(host_addr)`,
finds the port still held by the old container's mapping, and silently skips.
The old iptables DNAT rule (pointing to the dead container's IP) persists.

### Root Cause

`daemon.rs:395-397` — unconditional skip on allocation conflict:

```rust
if state.ports.is_allocated(host_addr).await {
    tracing::warn!(host.addr = %host_addr, "address already allocated, skipping");
    continue;
}
```

No attempt to check whether the port-holder is actually still alive.

### Approach

Extract a pure helper `resolve_stale_container()` that reads `daemon_state.mapping`
and identifies if the port's owner differs from the new container. If so, call
`on_container_stop(stale_id)` to clean up the old mapping, then fall through to
normal allocation.

**Why this works:**
- `on_container_stop` already handles deallocation, iptables removal, and persistence
- No changes to `PortAllocator` or `DaemonState` data structures
- Only lookup uses existing `HashMap<String, Vec<DockerPortMap>>`

### Changes

| # | File | Change |
|---|---|---|
| 1 | `crates/natmap/src/daemon.rs` | Add `resolve_stale_container()` helper function |
| 2 | `crates/natmap/src/daemon.rs` | Modify `on_container_start()` — replace skip with stale-resolution + retry |
| 3 | `crates/natmap/src/daemon.rs` | Add unit tests for `resolve_stale_container()` |

### Code Sketch

**Helper** (pure, no Docker/filesystem):

```rust
async fn resolve_stale_container(
    daemon_state: &Arc<RwLock<DaemonState>>,
    host_addr: SocketAddr,
    new_container_id: &str,
) -> Option<String> {
    let lock = daemon_state.read().await;
    lock.mapping.iter().find_map(|(id, maps)| {
        (id.as_str() != new_container_id
            && maps.iter().any(|m| m.request.host_addr == host_addr))
        .then(|| id.clone())
    })
}
```

**Modified `on_container_start` loop** (~daemon.rs:394-397):

```rust
if state.ports.is_allocated(host_addr).await {
    if let Some(stale_id) =
        resolve_stale_container(&state.daemon_state, host_addr, &container_id).await
    {
        tracing::info!(host.addr = %host_addr, stale.container.id = %stale_id,
            "port held by stale container, removing old mapping");
        self.on_container_stop(stale_id).await;
    } else {
        tracing::warn!(host.addr = %host_addr, "address already allocated, skipping");
        continue;
    }
}
```

Flow after deallocation: the loop continues to `state.ports.allocate(host_addr)`,
which now succeeds (port was released by `on_container_stop`).

### Tests

All tests for `resolve_stale_container` are inline unit tests (no Docker,
no filesystem, no iptables — pure state reads).

Per guidelines: "A unit is a behavior, not a method" — the behavior is
"find which container (if any) owns this port, excluding the caller."

| Test | Scenario | Assertion |
|---|---|---|
| `resolve_stale_returns_none_when_no_mapping` | Empty state | `None` |
| `resolve_stale_returns_none_when_no_match` | State has different port | `None` |
| `resolve_stale_returns_stale_id_when_match` | Different container owns the port | `Some("stale_id")` |
| `resolve_stale_returns_none_for_same_container` | Same container ID owns the port (duplicate event) | `None` |
| `resolve_stale_returns_first_with_multiple_stale` | Multiple containers, one matches | `Some(matching_id)` |

Each test:
- Creates an `Arc<RwLock<DaemonState>>` directly (no `Daemon` needed)
- Pre-populates with `DockerPortMap` entries
- Calls `resolve_stale_container()` and asserts the result

### Verification

```bash
cargo test -p lab-ops_natmap        # unit tests (existing + new)
cargo test -p lab-ops --test natmap_docker  # Docker integration tests (existing)
./dev.sh format && ./dev.sh lint    # style checks
```

### Notes

- No changes to `PortAllocator`, `DaemonState`, `DockerPortMap`, or any type
- The `on_container_stop` call inside `on_container_start` is safe:
  - We hold no locks when calling it (the read lock from `resolve_stale_container` is dropped before return)
  - `on_container_stop` acquires its own write lock, removes all stale mappings, deallocates ports, persists
- Edge case: if the new container exposes **multiple ports**, each port hit in the loop will call `on_container_stop` for the same stale container. The **first** call removes all mappings; subsequent calls return immediately at the `lock.mapping.remove(&container_id)` check (`on_container_stop:371`). This is safe — no-op on second call.
- Edge case: what if `resolve_stale_container` returns `Some` but the container is legitimately still running? This would require two Docker containers binding the same host port, which Docker itself prevents at container creation time. Not a real scenario.
