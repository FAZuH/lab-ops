# Testing Strategy

See [standards.md §7](standards.md#7-testing) for naming conventions, test location rules, and fixture patterns.

## Test Suites

### Run Commands

```bash
cargo test --workspace                              # All tests (excl. Docker)
cargo test -p lab-ops_lab-lib                       # lab-lib crate only
cargo test -p lab-ops_natmap                        # natmap crate only
cargo test -p lab-ops_auto-discover                 # auto-discover crate only
cargo test -p lab-ops --test natmap_docker          # natmap Docker integration tests
cargo test -p lab-ops --test auto_discover          # auto-discover Docker integration tests
cargo test -p lab-ops_natmap --test model           # natmap model integration tests
cargo test --doc -p lab-ops_natmap -p lab-ops_lab-lib  # Doc tests only
```

### Unit Tests (Inline)

Located in `#[cfg(test)] mod tests { }` blocks within source files.

| File | Tests | Covers |
|---|---|---|
| `src/cmd/cf2ansible.rs` | 4 | DNS zone parsing, record extraction, YAML output |
| `src/cmd/cf2terra.rs` | 1 | Terraform HCL output |
| `src/cmd/dockernet.rs` | 8 | IP formatting and port bind parsing |
| `crates/lab-lib/src/port.rs` | 11 | Port allocation, persistence, free-port checks |
| `crates/natmap/src/api.rs` | 22 | HTTP handlers, `parse_socket_addrs` boundary/edge cases |
| `crates/natmap/src/daemon.rs` | 2 | Tracing span fields on daemon ops |
| `crates/auto-discover/src/config.rs` | 2 | preserve_src_ip config propagation (defaults, overrides) |
| `crates/auto-discover/src/consul.rs` | 5 | Consul service registration, metadata, URL encoding |
| `crates/auto-discover/src/daemon.rs` | 2 | Tracing span fields on container events |
| `crates/auto-discover/src/forwarding.rs` | 27 | `group_forwarding_services`, `parse_dnat_rule`, 5 proptest invariants, edge cases |


**Total: 88 inline unit tests**

### Doc Tests

Located in `/// ```rust` documentation blocks.

| Crate | Tests | Covers |
|---|---|---|
| `lab-ops_lab-lib` | 1 | TransportProtocol parse/display round-trip |
| `lab-ops_natmap` | 4 | `output_dnat_destination`, `DockerPortMapRequest`, `DockerPortMap::new`, `parse_docker_mapping` |

**Total: 5 doc tests**

### Property-Based Tests

Documented inline with their parent modules. Proptest invariants live alongside the code they test (e.g., `forwarding.rs`), using the `proptest!` macro with `#[test] fn` syntax.

| Location | Tests | Covers |
|---|---|---|
| `crates/auto-discover/src/forwarding.rs` | 5 | `group_forwarding_services`: dedup keys, sorted ports, all ports in group, hairpin/preserve_src_ip OR |
| `crates/natmap/tests/cli.rs` | 8 | `parse_docker_mapping`: 1- to 5-part format roundtrips |

`service_matches_group` helper is defined outside the `proptest!` block (the macro doesn't support mixing regular functions with test functions).

### Integration Tests (External)

Located in `tests/` or `crates/*/tests/` directories.

| File | Tests | Covers |
|---|---|---|
| `tests/cf2ansible.rs` | 6 | cf2ansible end-to-end (zone file → YAML output) |
| `crates/natmap/tests/cli.rs` | 27 | Port mapping string parsing via `parse_docker_mapping`, 8 proptest roundtrips, edge cases |
| `crates/natmap/tests/model.rs` | 12 | Model serialization, rule comment, `output_dnat_destination` |

**Total: 45 integration tests**

### Docker Integration Tests

Located in `tests/natmap_docker.rs` (34 tests) and `tests/auto_discover/` (60 tests across 7 modules), behind `#[cfg(feature = "docker-tests")]`.

**natmap Docker tests** (`tests/natmap_docker.rs`, 34 tests):
Spins up a privileged Ubuntu container with iptables, runs the natmap daemon, and verifies iptables NAT rule creation/removal, startup flush, graceful shutdown, policy routing, and port allocation via CLI commands over the Unix socket.

| Category | Count |
|---|---|
| NAT rule operations (DNAT, SNAT, hairpin, forward) | 5 |
| Rule cleanup (clear, container flush) | 6 |
| Startup flush (natmap chains, postrouting, output) | 6 |
| Port management (freebind, release, conflict) | 3 |
| Graceful shutdown | 3 |
| Policy routing | 2 |
| Other | 9 |

**auto-discover Docker tests** (`tests/auto_discover/`, 60 tests across 7 modules):
Spins up a privileged Docker container running Consul, natmap, and auto-discover daemons. Verifies Consul registration, port binding, forwarding metadata, crash recovery, config change handling, registration metadata, forwarding sync (DNAT rules), concurrency, and large configs.

| Module | Tests | Covers |
|---|---|---|
| `forwarding.rs` | 9 | DNAT sync, duplicate/stale rules, multi-port, hairpin, preserve_src_ip hairpin |

| `port_binding.rs` | 7 | Static/ephemeral ports, bind_ip, bind_interface, local forwarding |
| `registration.rs` | 12 | Consul registrations, container events, domain slug, extra fields, concurrent starts |
| `recovery.rs` | 11 | Config changes, crash recovery, YAML validation, pre/postprocess, config sync |
| `local_services.rs` | 4 | Local service type, forwarding remote, reachability, combined rproxy+forwarding |
| `preserve_src_ip.rs` | 6 | Global/per-service preserve_src_ip, policy route, idempotency, cleanup |

**Total: 94 Docker tests**

The Docker image is built once via `Once` from `ubuntu:24.04` with `iptables` installed.

## How Tests Run

`./dev.sh test` executes `cargo test --workspace --all-targets --all-features`. Note that `--all-features` enables `docker-tests` in the root crate and the auto-discover crate, so all test categories are included.

### Docker Test Requirements

Docker tests must run single-threaded:

```bash
cargo test --features docker-tests -- --test-threads=1
```

Each test creates a fresh Docker container. Parallel execution causes race conditions with image builds.

## Common Pitfalls

1. **Docker tests hang**: Always use `--test-threads=1`.
2. **Daemon connection refused**: Docker tests must start the daemon inside the container with `&` and `sleep 2` before CLI commands.
3. **Port binding fails in Docker**: Use `--privileged`. Port allocation requires `CAP_NET_BIND_SERVICE` or root.
4. **State file conflicts**: Each test uses a unique `--state-dir` path.

## Adding New Tests

### Inline Unit Test

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_name_scenario() {
        let result = thing_under_test(input);
        assert!(result.is_ok());
    }
}
```

### Integration Test (Non-Docker)

Add a `#[test]` function to `tests/` or `crates/*/tests/`. Use `env!("CARGO_BIN_EXE_lab-ops")` for binary tests.

### Docker Integration Test

1. Add a `#[test]` function to `tests/natmap_docker.rs` or the appropriate module in `tests/auto_discover/`
2. Use `run_in_docker(&[...])` for shell commands
3. Pattern: start daemon → wait → run command → verify output
