# Testing Strategy

See [standards.md §7](standards.md#7-testing) for naming conventions, test location rules, and fixture patterns.

## Test Suites

### Run Commands

```bash
cargo test --workspace                           # All tests (excl. Docker)
cargo test -p natmap                             # natmap crate only
cargo test -p auto-discover                      # auto-discover crate only
cargo test --features docker-tests -- --test-threads=1  # Docker integration tests
```

### Unit Tests (Inline)

Located in `#[cfg(test)] mod tests { }` blocks within source files.

| File | Tests | Covers |
|---|---|---|
| `src/cmd/cf2ansible.rs` | 20 | DNS zone parsing, record extraction, YAML output |
| `src/cmd/dockernet.rs` | 8 | IP formatting and port bind parsing |
| `crates/auto-discover/src/config.rs` | 18 | YAML config parsing, resolution, forwarding |
| `crates/auto-discover/src/consul.rs` | 5 | Consul service registration, metadata encoding |
| `crates/auto-discover/src/ports.rs` | 6 | Port allocation, persistence, free-port checks |

**Total: 57 inline unit tests**

### Integration Tests (External)

Located in `tests/` or `crates/*/tests/` directories.

| File | Tests | Covers |
|---|---|---|
| `tests/integration.rs` | 6 | cf2ansible end-to-end (zone file → YAML output) |
| `crates/natmap/tests/cli.rs` | 8 | Port mapping string parsing |
| `crates/natmap/tests/model.rs` | 11 | Model serialization, rule comment generation |

**Total: 25 integration tests**

### Docker Integration Tests

Located in `tests/natmap_docker.rs` and `tests/auto_discover.rs`, behind `#[cfg(feature = "docker-tests")]`.

**auto-discover Docker tests** (`tests/auto_discover.rs`, 19 tests):
Spins up a privileged Docker container (Ubuntu 24.04 + Consul + iptables) running Consul, natmap, and auto-discover daemons. Verifies Consul registration, nginx config KV storage, forwarding metadata, container lifecycle cleanup, crash recovery, natmap integration edge cases, config generation pipeline, and registration metadata.

| Category | Count |
|---|---|
| NAT rule operations (DNAT, SNAT, hairpin, forward) | 5 |
| Rule cleanup (clear, container flush) | 6 |
| Startup flush (natmap chains, postrouting, output) | 6 |
| Port management (freebind, release, conflict) | 3 |
| Graceful shutdown | 3 |
| Other | 5 |

**Total: 28 natmap Docker tests + 19 auto-discover Docker tests = 47 Docker tests**

The Docker image is built once via `Once` from `ubuntu:24.04` with `iptables` installed.

## How Tests Run

`./dev.sh test` executes `cargo test --workspace --all-targets --all-features`. Note that `--all-features` enables `docker-tests` in the root crate and the auto-discover crate, so the Docker tests listed below are included in `./dev.sh test`.

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

### Docker Integration Test

1. Add a `#[test]` function to `tests/natmap_docker.rs`
2. Use `run_in_docker(&[...])` for shell commands
3. Pattern: start daemon → wait → run command → verify output
