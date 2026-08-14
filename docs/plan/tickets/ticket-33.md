# 33 — Shared inspection module in lab-lib

**What to build:** lab-lib gains a shared container-inspection module: `DockerClient` (moved from auto-discover), a richer `ContainerInfo` carrying network settings including the container IP, and the rich inspect-parse (from natmap's `get_port_mappings`) as the one shape both crates read. Expand-only — existing code continues to compile; the parse is pinned by canned inspect fixtures.

**Blocked by:** None — can start immediately.

**Status:** ready-for-agent

- [ ] lab-lib exposes `DockerClient` and a `ContainerInfo` that carries the container IP and network settings
- [ ] The rich inspect parse (port mappings + target IP) lives in lab-lib and is unit-tested against canned bollard inspect JSON (with/without networks, multiple networks, empty fields)
- [ ] Existing natmap + auto-discover code compiles unchanged (expand-only)
- [ ] lab-lib + workspace unit suites green
