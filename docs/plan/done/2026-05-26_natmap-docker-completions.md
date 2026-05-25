## Plan: Docker Container Completions for `natmap docker`

### Update Log

| Date | Change |
|---|---|
| 2026-05-26 | Initial plan |
| 2026-05-26 | Implemented. All changes verified: build, format, lint, test (143 pass). Added completions for `add`, `rm`, `remap`, `ls` + `--name` flags. Manual test confirmed container names and IDs appear in completions. |
| 2026-05-26 | Updated `generate_completions` command to output the dynamic registration script directly. Users don't need to change their `eval "$(lab-ops completions ...)"` workflows. |

### Problem

`CONTAINER_ID` positional args in `natmap docker {add,rm,remap}` and `natmap ls` fall through to default file completions. Need to complete actual running Docker container names/IDs.

### Approach

Use `clap_complete`'s `unstable-dynamic` feature + `ArgValueCompleter`. The `completions` command will output dynamic registration scripts instead of static AOT scripts.

### Steps

| # | File | Change | Done |
|---|---|---|---|
| 1 | `Cargo.toml` | `clap_complete = { version = "4", features = ["unstable-dynamic"] }` | ✓ |
| 2 | `crates/natmap/Cargo.toml` | Add `clap_complete = "4"` | ✓ |
| 3 | `crates/natmap/src/completions.rs` | **New** — `complete_container_id()` — runs `docker ps --format '{{.Names}}\t{{.ID}}'`, parses names + short IDs, filters by prefix | ✓ |
| 4 | `crates/natmap/src/lib.rs` | Add `pub mod completions;` | ✓ |
| 5 | `crates/natmap/src/cli.rs` | Annotate `container_id` args with `ArgValueCompleter` on `Add`, `Remove`, `Remap`, `List`; also annotate `--name` flags | ✓ |
| 6 | `src/main.rs` | Add `CompleteEnv::with_factory(Cli::command).complete()` before `Cli::parse()` | ✓ |
| 7 | `src/main.rs` | Updated `generate_completions()` to output `EnvCompleter::write_registration` directly so users get dynamic completions seamlessly | ✓ |

### Verification

```bash
_CLAP_COMPLETE_INDEX=4 COMPLETE=bash lab-ops -- natmap docker add ""  # suggests containers
_CLAP_COMPLETE_INDEX=4 COMPLETE=bash lab-ops -- natmap docker rm ""   # suggests containers
_CLAP_COMPLETE_INDEX=4 COMPLETE=bash lab-ops -- natmap docker remap "" # suggests containers
_CLAP_COMPLETE_INDEX=3 COMPLETE=bash lab-ops -- natmap ls ""           # suggests containers
./dev.sh all                             # format → lint → test (143 pass)
```

### Notes

- Users' existing `eval "$(lab-ops completions zsh)"` workflows now automatically fetch dynamic completions!
- `std::process::Command("docker ps ...")` used instead of `bollard` to avoid needing async runtime at completion time.
- Both container names and IDs are returned as completion candidates.
