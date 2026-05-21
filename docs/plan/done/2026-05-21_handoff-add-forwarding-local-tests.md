# Handoff: Add ForwardingLocal Integration Tests

## Goal

Add integration tests for `ForwardingLocal` port type in the `lab-ops` auto-discover system. Currently tests exist for `ForwardingRemote` (kernel DNAT to proxy server) and `RProxy` (reverse proxy via NGINX), but `ForwardingLocal` has zero integration test coverage.

## Current State (Reference Context)

### Code Artifacts

- **Refactor plan**: `/home/fazuh/Projects/lab-ops/refactor-plan.md` — schema changes, struct definitions, merging logic
- **Config types**: `/home/fazuh/Projects/lab-ops/crates/auto-discover/src/config.rs` — `ForwardingConfig`, `ResolvedPortType::ForwardingLocal { bind_port, template, domains, ... }`
- **Daemon logic**: `/home/fazuh/Projects/lab-ops/crates/auto-discover/src/daemon.rs` — `sync_docker()` (line ~159) and `sync_local()` (line ~232) handle ForwardingLocal
- **Consul metadata**: `/home/fazuh/Projects/lab-ops/crates/auto-discover/src/consul.rs` — `build_consul_service` inserts `forwarding=true`, `forwarding_type=local`, and optionally `template` for ForwardingLocal
- **NGINX config**: `store_nginx_config` supports ForwardingLocal with non-empty template (same code path as RProxy)

### What ForwardingLocal Does

`binding_port: Some(bp)` → uses `bp` as static host port (no ephemeral allocation, no Consul port pool)
`binding_port: None` → falls through to ephemeral pool allocation

| Context | bind_port=Some | bind_port=None |
|---|---|---|
| Docker | static port, natmap called | ephemeral port, natmap called |
| Local | static port, natmap called (target_ip) | ephemeral port, natmap called (target_ip) |

Unlike `ForwardingRemote` (which adds Consul metadata for proxy server DNAT sync), `ForwardingLocal` creates the iptables DNAT rule directly on the service node. It's a simple static host port binding.

### Existing Test Patterns

All tests in `/home/fazuh/Projects/lab-ops/tests/auto_discover.rs` use the **legacy YAML format** (`name:` + `networks:`) which is auto-converted by the backward-compat parser in `config.rs`. Tests follow this structure:

```rust
#[test]
fn test_name() {
    let yaml = r#"
networks:
  - name: <svc-name>
    ...
"#;
    let script = format!(
        r#"{setup}
# bash test commands
{teardown}
"#,
        setup = test_setup(yaml, ""),  // or test_setup_ext(yaml, extra, flags)
        teardown = test_teardown(&[cname]),
        cname = cname,
    );
    let out = run(&script);
    assert_pass(&out, "description");
}
```

Helper functions:
- `test_setup(yaml, extra)` → starts consul + natmap + auto-discover daemon (`--no-forwarding --no-nginx`). Writes `name: int-test-node` header + gen-nginx script
- `test_setup_ext(yaml, extra, flags)` → same but with custom daemon flags
- `test_teardown(&[names])` → `docker rm -f` containers + kills bg jobs
- `run(script)` → builds Docker test image, runs script inside privileged container with Docker socket

The gen-nginx script outputs `proxy_pass http://${AUTO_DISCOVER_BIND_IP}:${AUTO_DISCOVER_HOST_PORT}/;`.

### Existing Forwarding Tests (for reference)

| Test | What it Tests |
|---|---|
| `forwarding_static_port` | ForwardingRemote: port=36000, forwarding meta, ext_ip meta |
| `forwarding_hairpin_meta` | ForwardingRemote: hairpin=true meta |
| `forwarding_no_kv_config` | ForwardingRemote: empty template → no nginx KV config |
| `local_forwarding_remote` | Local + ForwardingRemote: port=40000, addr, forwarding meta |
| `docker_rproxy_and_forwarding` | Docker + ForwardingRemote merged with RProxy: both template + forwarding meta |

### New Tests Added (for pattern reference)

| Test | File line | Format |
|---|---|---|
| `local_forwarding_remote` | ~2085 | `test_setup_ext`, legacy yaml, local_ip + forwarding |
| `docker_reachability` | ~2140 | `test_setup`, legacy yaml, curl reachability + iptables checks |
| `docker_rproxy_and_forwarding` | ~2184 | `test_setup`, legacy yaml, two networks with same name |

## Specific Gap to Fill

`ForwardingLocal` has **zero integration tests** for both Docker and Local contexts.

### Test Scenarios Needed

#### 1. Docker + ForwardingLocal with `bind_port` (static port)
```yaml
networks:
  - name: it-svc-fwd-local
    container_port: 80
    # No template → forwarding-only, no nginx config
    forwarding:
      ext_ip: 203.0.113.43
      ext_ports: [36000]    # Legacy: ext_ports[0] = host port
      proto: tcp
```
Legacy format limitation: the old `forwarding` block doesn't have a `type` field. The legacy converter always creates `ForwardingType::Remote`. To test `ForwardingType::Local`, you need to either:
- (a) Write the YAML in new format directly (bypassing `test_setup`) with `forwarding: [{ type: local, port: 80, bind_port: 36000 }]`
- (b) Modify the legacy parser to support an `fwd_type` field in the legacy forwarding block

Check which approach is less invasive. Option (a) requires writing the YAML file outside of `test_setup_ext` (write directly in the bash script or use `extra_setup` to overwrite).

#### 2. Local + ForwardingLocal with `bind_port` (static port)
```yaml
networks:
  - name: it-local-fwd-local
    local_ip: 10.99.99.99
    local_port: 5000
    forwarding:
      ext_ip: 203.0.113.43
      ext_ports: [50000]
      proto: tcp
```
Same limitation as above — legacy format creates `ForwardingType::Remote`.

### Verification Points

For each ForwardingLocal test:
1. ✅ Consul registration with correct port (`bind_port` value, not ephemeral)
2. ✅ Consul metadata: `forwarding=true`, `forwarding_type=local`
3. ✅ Natmap DNAT rule exists (iptables NATMAP chain)
4. ❌ No nginx KV config (if template is empty)
5. ✅ Container serving traffic (via `docker exec` for Docker tests)
6. ✅ ForwardingLocal with non-empty template generates nginx config

## Test Infrastructure Notes

- Container cleanup: stale test containers (`it-*`) persist on host after test crashes. Run `docker rm -f it-<name>` before each test
- Test `name:` YAML key (not `node:`) — the legacy format parser expects this
- All tests run with `--test-threads=1` (Docker integration tests can't run in parallel)
- The test image is cached at `lab-ops-auto-discover-test:latest`
- Tests take ~7-15 seconds each; full suite ~9 minutes

## Suggested Skills

The next agent should load these skills in order:

1. **`handoff`** — to understand how to read this document and continue the session properly
2. **`caveman`** — for terse mode when iterating on test bash scripts to minimize token waste
3. **`grill-me`** — if the approach for (a) vs (b) above is unclear, use this to work through the design decision

## Associated Files

- `/home/fazuh/Projects/lab-ops/tests/auto_discover.rs` — all integration tests
- `/home/fazuh/Projects/lab-ops/crates/auto-discover/src/daemon.rs` — sync_docker (ForwardingLocal handler ~line 159)
- `/home/fazuh/Projects/lab-ops/crates/auto-discover/src/config.rs` — ForwardingConfig, ResolvedPortType
- `/home/fazuh/Projects/lab-ops/crates/auto-discover/src/consul.rs` — build_consul_service ForwardingLocal arm
- `/home/fazuh/Projects/lab-ops/refactor-plan.md` — full schema and architecture documentation

## Quick Commands

```bash
# Run a single test
cargo test -p lab-ops --test auto_discover -- <test_name> --test-threads=1

# Run all integration tests
cargo test -p lab-ops --test auto_discover -- --test-threads=1

# Run all unit tests
cargo test -p auto-discover --lib

# Check compilation
cargo check -p lab-ops --test auto_discover

# Clean up stale test containers
docker rm -f it-reach it-combo it-local-fwd it-fwd it-hairpin it-fwd-nokv 2>/dev/null
```
