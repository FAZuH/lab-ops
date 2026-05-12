# Testing Strategy

## Test Categories

### Unit Tests

Located in `src/` files as `#[cfg(test)] mod tests { }` blocks and in `crates/natmap/tests/`.

```bash
# Run all unit tests
cargo test --workspace --all-targets

# Run natmap crate tests only
cargo test -p natmap
```

Current coverage:
- `cmd/cf2ansible.rs` — 19 tests for DNS zone parsing, record extraction, YAML output
- `cmd/dockernet.rs` — 10 tests for IP formatting and bind parsing
- `crates/natmap/tests/cli.rs` — 8 tests for port mapping string parsing
- `crates/natmap/tests/model.rs` — 9 tests for model serialization and rule comment generation

### Docker Integration Tests

Located in `tests/natmap_docker.rs`. These tests spin up a privileged Docker container running the actual `lab-ops` binary and verify end-to-end behavior.

```bash
# Run Docker integration tests (single-threaded required)
cargo test --features docker-tests -- --test-threads=1
```

Tested behaviors:
- `test_natmap_forward` — DNAT rule add via daemon, verify in iptables-save
- `test_natmap_snat` — SNAT rule add via daemon, verify in iptables-save
- `test_natmap_hairpin` — Hairpin rule add via daemon, verify in iptables-save

**Important:** Docker tests must run with `--test-threads=1` to avoid parallel container conflicts (multiple containers trying to use the same image simultaneously).

The test Docker image is built once (via `Once`) from `ubuntu:24.04` with `iptables` installed. Each test function creates a fresh Docker container that starts the daemon, runs the CLI command, and verifies the output.

### Adding New Docker Tests

1. Add a new `#[test]` function to `tests/natmap_docker.rs`
2. Use `run_in_docker(&[...])` to execute shell commands in the container
3. The pattern is: start daemon in background → wait → run command → verify output
4. Always use `--test-threads=1` when running

Example template:
```rust
#[test]
fn test_natmap_mytest() {
    let out = run_in_docker(&[
        "lab-ops natmap daemon --socket /tmp/ns --state-dir /tmp/st --socket-group root &",
        "sleep 2",
        "&&",
        "lab-ops natmap --socket /tmp/ns ...",
        "&&",
        "iptables-save",
        "|",
        "grep ...",
    ]);
    assert!(out.contains("expected-iptables-output"));
}
```

## Development Workflow

```bash
# Quick iteration loop
cargo build -p natmap           # Build only the natmap crate
cargo test -p natmap            # Run natmap unit tests
cargo test --features docker-tests -- --test-threads=1  # Run Docker tests

# Full pre-commit check
./dev.sh all                     # Format + lint + all tests (excluding Docker tests)

# Full check including Docker tests
./dev.sh format && ./dev.sh lint && cargo test --workspace --all-targets && cargo test --features docker-tests -- --test-threads=1
```

## Common Pitfalls

1. **Docker tests hang**: Always use `--test-threads=1`. Parallel container creation causes race conditions with the Docker image build.
2. **Daemon connection refused**: Docker tests must start the daemon inside the container with `&` and `sleep 2` before running CLI commands.
3. **Port binding fails in Docker**: Use `--privileged` for Docker test containers. Port allocation requires `CAP_NET_BIND_SERVICE` or root.
4. **State file conflicts**: Each test should use a unique `--state-dir` path to avoid state file collisions.
