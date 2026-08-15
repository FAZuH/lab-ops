# Spec: Container inspection into lab-lib

## Problem Statement

Container inspection is split across both crates in two incompatible shapes. auto-discover carries a `DockerClient` (bollard API) that produces a bare `ContainerInfo {id, name, compose_project}` with no network information, plus a `get_container_ip` helper that shells out to the docker CLI to read an IP. The natmap daemon independently parses the same docker inspect response (its own `get_port_mappings`) to extract ports and target IPs. The two crates parse the same live container data differently, one of them through a subprocess, so a change to the inspect shape is applied in two places and the CLI path is a second, slower code path for the same job.

## Solution

Move container inspection into lab-lib as a shared module: the `DockerClient`, a richer `ContainerInfo` that carries network settings (including the container IP), and the inspect-parsing logic natmap already has (port mappings + target IP) promoted into it. Both crates consume the one inspect API; the CLI-shelled `get_container_ip` is deleted in favor of the same bollard network-settings read natmap already uses. lab-lib's existing `docker` module (`connect`, `trim_container_name`) is the foundation it lands on.

## User Stories

1. As the auto-discover daemon, I want to read a container's IP from the bollard inspect response instead of spawning the docker CLI, so that inspection is one fast, uniform path.
2. As the auto-discover daemon, I want `ContainerInfo` to carry network settings, so that I no longer need a second call to learn an IP.
3. As the natmap daemon, I want my inspect parsing (port mappings + target IP) shared from lab-lib, so that the format is defined once and consumed by both crates.
4. As a test, I want inspection unit-tested against canned bollard inspect fixtures in lab-lib, so that the parse shape is pinned where it is defined.
5. As the maintainer, I want a container's inspect shape changed in one place, so that the two crates can never drift apart again.

## Implementation Decisions

- **lab-lib gains a shared inspection module** on top of its existing `docker` module: `DockerClient` moves here, `ContainerInfo` grows to carry network settings (container IP), and the inspect-parsing logic natmap already has (port mappings + target IP) is promoted into it as the one rich inspect shape both crates read.
- **`ContainerInfo` grows.** It gains the container IP (and whatever network fields the rich inspect parse needs). The ripple into `container_matches` and both crates' tests is in scope.
- **`get_container_ip` CLI path is deleted.** The IP comes from the shared inspect API (bollard network settings), the same source natmap already uses.
- **natmap's `get_port_mappings` parsing merges into the lab-lib inspect API.** One rich shape, both crates read the same type; natmap keeps a thin local wrapper where it needs a crate-specific view.
- **auto-discover drops its local `DockerClient` and bare `ContainerInfo`** in favor of the lab-lib types and the shared inspect call.

## Testing Decisions

- A good test pins the inspect-shape parsing against fixed fixtures: canned bollard inspect JSON -> the shared `ContainerInfo`/port-mapping model, asserted field-by-field.
- **lab-lib**: unit tests over canned inspect fixtures (with and without networks, multiple networks, empty fields), covering the parse of IP, port mappings, and target IP.
- **auto-discover**: existing sync tests keep passing against the shared types via the existing fake adapters; `container_matches` tests cover the grown `ContainerInfo`.
- **natmap**: the port-mapping flow tests re-point at the lab-lib inspect API through the existing fake iptables/ports adapters.
- Prior art: the `parse_docker_inspect_output` fixtures in `crates/auto-discover/src/docker.rs` tests, and the port-mapping parse tests in the natmap crate.

## Out of Scope

- Container inspection over anything other than the local docker daemon (no remote registries, no container orchestration).
- Changing how docker is connected (`DOCKER_HOST` handling stays as-is in lab-lib's `connect`).
- Candidate 4 (single port authority) — separate spec.

## Further Notes

- lab-lib already has a `docker` module (`connect`, `trim_container_name`); this spec builds on it rather than starting a new module from scratch.
- The two crates currently parse the same inspect data in two shapes; this spec unifies on one rich shape defined in lab-lib.
- CONTEXT.md vocabulary: port mapping, reconcile. The domain term "port mapping" is defined as the association from host to container; the shared inspect shape feeds that model.
