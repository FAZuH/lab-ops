# 34 — natmap consumes the shared inspect API

**What to build:** natmap drops its local `get_port_mappings` parsing and reads the shared lab-lib inspection shape for its mapping flow. Behavior is unchanged; the format is defined once in lab-lib.

**Blocked by:** #33 — the shared inspection module must land first.

**Status:** ready-for-agent

- [ ] natmap's mapping flow uses the lab-lib inspect API; no local inspect-parse duplication remains
- [ ] Port-mapping tests re-point at the shared shape; behavior unchanged
- [ ] natmap lib suite green
