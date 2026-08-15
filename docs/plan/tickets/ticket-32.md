# 32 — auto-discover stops owning ports; PortAssignments deleted

**What to build:** auto-discover no longer decides port availability locally. `decide_ports` becomes a pure translation of desired intent into a mapping request; all availability answers come from the natmap daemon via the `NatmapOps` seam. On a 409 conflict for a local ForwardRemote target, the service is still registered in Consul without the mapping (skip-but-register). The per-pass `PortAssignments` load/save and the local `is_port_free` bind-check are removed, and the now-orphaned `PortAssignments` type is deleted from lab-lib.

**Blocked by:** #31 — the daemon must be able to allocate on the mapping flow first.

**Status:** ready-for-agent

- [ ] `decide_ports` issues mapping requests to the daemon (no local availability decision); no `is_port_free` bind-check remains in auto-discover
- [ ] `PortAssignments` is no longer loaded/saved by auto-discover and the type is deleted from lab-lib (including any remaining consumers)
- [ ] 409 conflict on a local ForwardRemote target → service still registered in Consul (skip-but-register)
- [ ] `sync` command path and container-event path behave identically via `sync_service`
- [ ] Unit-tested against an in-memory `NatmapOps` fake configured to report conflicts; auto-discover + natmap + root lib suites green
