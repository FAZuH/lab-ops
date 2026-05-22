# auto-discover Integration Test Plan

Ports the existing `crates/auto-discover/tests/integration.sh` to Rust and defines additional
test scenarios for end-to-end validation of the auto-discover daemon.

## Test Infrastructure

- **Feature-gated**: `#[cfg(feature = "docker-tests")]` in `crates/auto-discover/tests/integration.rs`
- **Docker image**: `ubuntu:24.04` + iptables + Consul + curl + jq (built once per test run via `Once`)
- **Execution**: Each test runs a self-contained shell script inside a `--privileged` Docker container
  with the `lab-ops` binary and Docker socket mounted
- **Daemons per container**: Consul (dev mode), natmap daemon, auto-discover daemon (discovery-only by default)
- **Verification**: curl to Consul agent API + `lab-ops natmap ls` for natmap state
- **Test naming**: `<category>_<scenario>` (matching the existing `tests/natmap_docker.rs` convention)

Run:
```bash
cargo test -p auto-discover --features docker-tests -- --test-threads=1
```

## Phase 1 — Ported from integration.sh (COMPLETE)

Tests A–I verify core discovery, forwarding, nginx config generation, and cleanup.

| Test | Category | What it verifies |
|---|---|---|
| `default_binding_all_interfaces` | A | Container registers with Consul, natmap maps to `0.0.0.0:<ephemeral>` |
| `bind_ip_strict_address` | B | `bind_ip: 10.99.99.1` → natmap binds to that specific IP |
| `bind_interface_resolved_address` | C | `bind_interface: dummy0` → resolved to interface IP (10.99.99.1) |
| `forwarding_static_port` | D | `forwarding.ext_ports[0]` used as static port, forwarding meta registered |
| `forwarding_hairpin_meta` | E | `forwarding.hairpin: true` → `Meta.hairpin="true"` in Consul |
| `nginx_config_kv_write` | F | Nginx config stored to `nginx-configs/sites/{id}.conf` in KV |
| `nginx_config_private_service_placeholder` | G | `HTTP_PROXY` template → custom IP placeholder in config |
| `forwarding_no_kv_config` | H | Forwarding-only service (empty template) has no nginx KV entry |
| `container_stop_kv_delete_and_deregister` | I | Stop container → KV key deleted + Consul service deregistered |

## Phase 2 — Crash Recovery (COMPLETE)

Tests that verify the daemon gracefully recovers from component failures.

| Test | What it verifies |
|---|---|
| `restart_auto_discover_picks_up_missed_containers` | Start container while auto-discover is NOT running, then start auto-discover → sync picks up the container and registers it |
| `restart_natmap_new_container_registered_after_recovery` | Kill natmap, start container (auto-discover fails to register), restart natmap, start another container → registers successfully |

## Phase 3 — Config Change Handling (COMPLETE)

Tests that verify the daemon reacts correctly to discovery.yaml changes.

| Test | What it verifies |
|---|---|
| `add_service_to_config_picked_up_on_sync` | Add a new network entry to discovery.yaml while daemon is running. Run sync → new service registered |
| `remove_service_from_config_stale_deregistered` | Remove a network entry. Run sync → stale Consul service deregistered |
| `change_bind_ip_service_reregisters` | Change `bind_ip` in config. Old service deregistered, new one registered with updated address |
| `invalid_yaml_config_daemon_warns_not_crash` | Write invalid YAML to discovery.yaml. Sync fails gracefully |
| `remove_all_services_clean_slate` | Empty `networks: []`. All services deregistered |
| `change_nginx_generator_path_missing` | Point `nginx_generator` to nonexistent script. Registers Consul service but skips KV config |

## Phase 4 — Natmap Integration (COMPLETE)

Tests that exercise the natmap port mapping and edge cases.

| Test | What it verifies |
|---|---|
| `host_networked_container_skipped` | Container with `--network host` → no Docker-managed ports → not matched, no registration |
| `wrong_exposed_port_skipped` | Container in matching project but exposing port 80 when config expects 9999 → not matched |

## Phase 5 — Nginx Config Generation (COMPLETE)

Tests for the config generator pipeline and KV storage.

| Test | What it verifies |
|---|---|
| `stream_template_stored_in_streams_prefix` | `template: TCP_PROXY` → config stored at `nginx-configs/streams/{id}.conf` not `sites/` |
| `extra_fields_passed_to_consul_meta` | `extra: { cluster: "us-east", max_conns: "100" }` → `Meta.cluster` and `Meta.max_conns` in Consul |
| `preprocess_script_modifies_config` | Inline `preprocess` script piped after generator. Output in KV reflects preprocess changes |
| `postprocess_script_stored_in_kv` | `postprocess` set → `.postproc` key stored alongside `.conf` in KV |
| `multi_domain_config_all_domains_in_env` | Multiple domains → primary domain in `Meta.domain`, config references primary domain |
| `generator_fails_daemon_warns` | Generator script path missing. Daemon warns, Consul service still registered |

## Phase 6 — Consul Registration Details (COMPLETE)

Tests for service registration metadata and lifecycle.

| Test | What it verifies |
|---|---|
| `service_id_contains_domain_slug` | Service ID format: `{node_name}-{domain_slug}-{port}`. Dots in domain → dashes in slug |
| `service_id_no_domain_falls_back_to_name` | Service without `domains` → ID uses `{node_name}-{service_name}-{port}` |
| `container_id_in_consul_meta` | `Meta.container_id` set to Docker container ID prefix |

## Phase 7 — Forwarding Sync (COMPLETE)

Tests for the proxy-side `forwarding-sync` one-shot and daemon component.

| Test | What it verifies |
|---|---|
| `forwarding_sync_applies_dnat_rules` | Services registered with `forwarding: true` → `forwarding-sync` applies DNAT rules via natmap |
| `forwarding_sync_removes_stale_rules` | Deregister a forwarding service → next sync removes its DNAT rules |
| `no_forwarding_services_sync_noop` | Zero forwarding services in Consul → `forwarding-sync` no-ops, no errors |
| `forwarding_group_multiple_ports` | Service with `ext_ports: [36005,36006,36007]` → all ports DNAT'd |

## Phase 8 — Nginx Daemon Component (COMPLETE)

Tests for the proxy-side nginx config sync (the nginx component of the unified daemon).

Verification uses file system checks (no nginx installation required — the daemon's `nginx -t` and `systemctl reload` steps fail gracefully when nginx is not installed).

| Test | What it verifies |
|---|---|
| `nginx_daemon_writes_config_to_disk` | Configs in KV → daemon writes to /var/lib/auto-discover/nginx-configs |
| `nginx_daemon_creates_symlinks` | Config file → symlink in sites-available |
| `nginx_daemon_runs_postproc` | `.postproc` KV key → postproc script applied before writing |
| `nginx_daemon_common_postprocs` | Scripts in `/etc/auto-discover/postprocs.d/` → applied in lexicographic order |
| `nginx_daemon_stale_cleanup` | KV key deleted → symlink and config file removed on next sync |
| `nginx_daemon_full_cycle` | End-to-end: KV → file → symlink |

## Phase 9 — Edge Cases & Stress (COMPLETE)

| Test | What it verifies |
|---|---|
| `container_restart_reuses_port_from_state` | Stop container, start again with same project/port. Same host port reused from ports.json |
| `container_die_event_deregistration` | `docker kill` (SIGKILL) → service deregistered without waiting for full sync |
| `compose_project_mismatch_skipped` | Container with `com.docker.compose.project=foo` but config has no `foo` entry → skipped |
| `concurrent_starts_all_registered` | Start 5 containers simultaneously with unique compose projects → all registered |
| `large_config_many_services` | Config with 5 network entries → all parsed, matched correctly |

## Files

| File | Purpose |
|---|---|
| `crates/auto-discover/tests/integration.rs` | Integration tests (feature-gated) — note: actual file is `tests/auto_discover.rs` in workspace root |
| (removed) `crates/auto-discover/tests/integration.sh` | Shell test — superseded by integration.rs |

## Related Docs

See `docs/dev/testing.md` for Docker test requirements and `docs/dev/standards.md` §7 for test conventions.
