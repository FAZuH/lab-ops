# 35 — auto-discover consumes the shared inspect API

**What to build:** auto-discover drops its local `DockerClient` and bare `ContainerInfo`, uses the lab-lib types, and deletes the CLI-shelled `get_container_ip` in favor of the bollard network-settings read in the shared inspect. `container_matches` and the sync tests cover the grown `ContainerInfo`; behavior is unchanged.

**Blocked by:** #33 — the shared inspection module must land first.

**Status:** ready-for-agent

- [ ] Local `DockerClient` + bare `ContainerInfo` removed; lab-lib types used throughout
- [ ] `get_container_ip` CLI path deleted; the container IP comes from the shared inspect (bollard network settings)
- [ ] `container_matches` and sync tests cover the grown `ContainerInfo`; behavior unchanged
- [ ] auto-discover lib suite green
