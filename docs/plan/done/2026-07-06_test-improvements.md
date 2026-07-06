# Test Improvements — Completed & Planned

## Completed

### Phase 1 — Test Hygiene
- **Deleted** `crates/natmap/src/tracing_fields.rs` (dead file, not in `lib.rs`)
- **Removed tautological test** — `output_dnat_destination` helper in `tests/model.rs` was a copy of production logic. Extracted as `pub fn` in `iptables.rs`, tested via doc tests instead.
- **Fixed logic-in-tests** — Split `config.rs` single test with `if/else` + `panic!` into 2 focused tests with extracted helpers.
- **Removed for-loop tests** — `cf2ansible.rs`: `all_files_no_data_block` and `all_files_api_token_in_every_task` replaced by per-file checks in `assert_common`.

### Phase 2 — New Unit & Integration Tests
- **`api.rs`**: 20 tests covering `parse_socket_addrs`, `list_mappings`, add/remove DNAT/SNAT/hairpin, add/remove mapping (by port/id), `remap_port`, `clear_all`, `remove_policy_route`.
- **`command.rs`**: Extracted `parse_docker_mapping()` as a public fn. `tests/cli.rs` rewritten to call the real function (12 tests, was 11).

### Phase 3 — Docker Test Timing Fixes
- **`natmap_docker.rs`**: 45x `sleep 2` → socket polling loop, 3x `kill+silent wait` → PID polling loop.
- **`auto_discover.rs`**: `kill %3 %2 %1` → `jobs -p` based teardown.

### Phase 4 — Doc Tests
- 5 doc tests: `TransportProtocol`, `DockerPortMapRequest`, `DockerPortMap::new`, `parse_docker_mapping`, `output_dnat_destination`.

### Phase 5 — Documentation
- `docs/dev/testing.md` updated with accurate counts (78 unit, 5 doc, 30 integration, 94 Docker), corrected crate names, run commands.

---

## Next Steps

### 1. Property-Based Tests (Phase 3 from original plan)
- Add `proptest` as dev-dependency to `lab-ops_natmap`
- Test: `parse_docker_mapping` round-trip (parse → serialize → compare)
- Test: `group_forwarding_services` invariants (ports deduped, proto preserved, grouped by `(ext_ip, int_ip)`)
- Test: `TransportProtocol` round-trip (parse → display → parse)

### 2. Test Data Quality & Boundary Tests (Phase 5)
- Replace unrealistic values: `"test-svc"`, `"test-node"`, `"abc123"` with realistic ones
- Add boundary tests: port `0`, `65535`, IP `255.255.255.255`, empty string, unicode
- Test error messages contain expected text (not just error code)

### 3. Fix Pre-existing `unwrap()` in `command.rs:541`
- `p.split('/').next().unwrap()` → `p.split('/').next().ok_or_else(|| eyre!(...))?`

### 4. Split `auto_discover.rs` (2995 lines → logical modules)

**Current state**: Single file `tests/auto_discover.rs` containing all 60 Docker integration tests. Setup/teardown/run helpers at top, then 60 inline test functions. Tests are organized by function name prefix conventions, not file structure.

**Proposed split structure** (in `tests/auto_discover/`):

```
tests/
  auto_discover/
    mod.rs           — shared helpers: setup_image(), run(), teardown(), assert_pass(), new_format_setup()
    registration.rs  — consul registration, metadata, service checks
    forwarding.rs    — DNAT rule sync, forwarding metadata
    nginx.rs         — nginx config generation, pipeline, file ops
    recovery.rs      — crash recovery, config change handling, concurrency
    large_config.rs  — large config stress tests
```

**Module responsibilities:**

| Module | Tests (est) | Lines (est) |
|---|---|---|
| `mod.rs` | 0 (helpers) | ~120 |
| `registration.rs` | ~15 | ~600 |
| `forwarding.rs` | ~12 | ~600 |
| `nginx.rs` | ~15 | ~700 |
| `recovery.rs` | ~12 | ~500 |
| `large_config.rs` | ~6 | ~400 |

**Split strategy:**
1. Create `tests/auto_discover/` directory
2. Move helpers to `mod.rs` (make `pub(crate)`)
3. Categorize each test by its function name into the right module
4. Update `run()`/`teardown()` calls — they reference `setup_image()` and other helpers, so `use super::*` in each child module
5. No logic changes, purely mechanical file split
6. Delete old `tests/auto_discover.rs`

**Risks:**
- Feature-gated `#[cfg(feature = "docker-tests")]` needs to wrap `mod.rs` not each child
- Static `INIT: Once` must stay in `mod.rs` to avoid multiple initialization
- Tests may reference each other's helpers (verify before splitting)
