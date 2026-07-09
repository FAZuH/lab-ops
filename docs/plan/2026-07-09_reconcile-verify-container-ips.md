## Plan: `reconcile_docker_portmaps` — re-verify container IPs

### Problem

`reconcile_docker_portmaps()` at `daemon.rs:431-486` re-installs iptables rules
for surviving Docker containers using the IP address stored in `state.json`. It
never re-inspects the container to verify the IP hasn't changed.

If a Docker network was recreated (e.g., `docker network rm && docker network create`)
while natmap was down, a container may have the same `container_id` but a new
IP address. After restart, natmap installs DNAT rules pointing to the old IP —
traffic is blackholed.

### Root Cause

`daemon.rs:458-474` — the inner loop uses `m.request.container_addr` directly
from the persisted state without checking the current container IP:

```rust
for m in maps {
    let host_addr = m.request.host_addr;
    // ... allocate port ...
    let _ = iptables.install_dockermap(&m);
    // m.request.container_addr is stale
    kept.push(m);
}
```

### Approach

Between determining a container survived (line 452 check) and iterating its
port mappings, call `docker::get_port_mappings()` to get the current container
IPs. Build a lookup map by `host_addr`, then update each stored mapping's
`container_addr` before installing the iptables rule.

Extract a pure helper `reconcile_container_addr()` that handles the update
decision (unit testable without Docker).

### Changes

| # | File | Change |
|---|---|---|
| 1 | `crates/natmap/src/daemon.rs` | Add `reconcile_container_addr()` helper (pure, no IO) |
| 2 | `crates/natmap/src/daemon.rs` | In surviving-container loop, re-inspect container IP and update mapping before install |
| 3 | `crates/natmap/src/daemon.rs` | Add unit tests for `reconcile_container_addr()` |

### Code Sketch

**Helper** (pure):

```rust
/// Updates `stored` container address from `current` if it changed.
/// Returns `true` if an update was made.
fn reconcile_container_addr(
    stored: &mut DockerPortMapRequest,
    current: &DockerPortMapRequest,
) -> bool {
    if stored.container_addr != current.container_addr {
        tracing::info!(
            old.ip = %stored.container_addr,
            new.ip = %current.container_addr,
            host.port = %stored.host_addr.port(),
            "container IP changed, updating mapping"
        );
        stored.container_addr = current.container_addr;
        true
    } else {
        false
    }
}
```

**Modified surviving-container loop** (~daemon.rs:456-475):

```rust
// Re-inspect container to verify current IPs
let current_addrs: HashMap<SocketAddr, SocketAddr> = docker::get_port_mappings(docker, &id).await
    .ok()
    .map(|mappings| {
        mappings.into_iter()
            .map(|m| (m.request.host_addr, m.request.container_addr))
            .collect()
    })
    .unwrap_or_default();

let mut kept = Vec::new();
for mut m in maps {
    let host_addr = m.request.host_addr;

    // Update container_addr from live inspect if available
    if let Some(&current_ctn_addr) = current_addrs.get(&host_addr) {
        let old_container_addr = m.request.container_addr;
        m.request.container_addr = current_ctn_addr;
        if old_container_addr != current_ctn_addr {
            tracing::info!(container.id = %id, host.port = %host_addr.port(),
                old.ip = %old_container_addr, new.ip = %current_ctn_addr,
                "container IP changed on reload");
        }
    }

    // existing allocation + install logic...
    if ports.is_allocated(host_addr).await { ... }
    ...
}
```

Note: the inspect call is moved **outside** the inner per-mapping loop — one
Docker API call per container, not per port.

### Tests

**Helper unit tests** — `reconcile_container_addr()` is pure:

| Test | Scenario | Returns | State after |
|---|---|---|---|
| `reconcile_addr_no_change` | Same container_addr | `false` | Unchanged |
| `reconcile_addr_updated` | Different container_addr | `true` | Updated to new value |
| `reconcile_addr_same_ip_different_port` | Different host port but same container IP | `false` | Unchanged |

**Integration test** — verifying the full re-inspect + update + reinstall flow
requires Docker API access. Same Docker-in-Docker limitation as Bug 2a. Not
included in this PR's scope.

### Edge Cases

- **Transient Docker API failure**: `get_port_mappings()` returns `Err`. The
  `ok()` + `unwrap_or_default()` pattern silently falls back to stored IPs,
  preserving existing behavior.
- **Container with no current mappings for a stored host port**: not found in
  `current_addrs`, stored IP preserved unchanged. This could happen if the
  container's port configuration changed (ports removed) — the iptables rule
  still uses the old IP, but the mapping will be removed on the next container
  stop event anyway.
- **Multiple mappings per container**: one API call, N updates. Efficient.

### Verification

```bash
cargo test -p lab-ops_natmap        # unit tests
cargo test -p lab-ops --test natmap_docker  # integration tests (existing)
./dev.sh format && ./dev.sh lint
```
