# Structured Logging Guidelines

## 1. Structured fields — no string interpolation in messages

The message is a static label. All variable data goes in fields.

```rust
// WRONG
info!("Docker event received: {} on container {}", action, id);

// CORRECT
info!(
    container.id = %id,
    container.name = %name,
    event.action = %action,
    event.type   = %typ,
    "docker event received"
);
```

Field sigils:
- `%value` — Display (strings, IDs, IPs, ports)
- `?value` — Debug (structs/enums you don't control)
- bare value — primitives (bool, u16, etc.)

## 2. Standard field names

| Field | Value | Example |
|---|---|---|
| `container.id` | Container ID (full or truncated) | `%container_id` |
| `container.name` | Container name | `%name` |
| `event.action` | Docker event action string | `%action` |
| `compose.project` | Docker Compose project name | `%compose_project` |
| `host.addr` | Host socket address (IP:port) | `%host_addr` |
| `host.port` | Host port number | `host_port` (bare u16) |
| `container.addr` | Container socket address | `%container_addr` |
| `container.port` | Container port number | `container_port` (bare u16) |
| `ext.ip` | External IP address | `%ext_ip` |
| `int.ip` | Internal IP address | `%int_ip` |
| `proto` | Transport protocol (tcp/udp) | `%proto` |
| `service.id_prefix` | Service ID prefix for matching | `%prefix` |
| `service.id` | Full service ID | `%id` |
| `services.count` | Count of services | `.len()` (bare usize) |
| `services.active` | Active service count | `.len()` (bare usize) |
| `mappings.count` | Number of Docker port mappings | `.len()` (bare usize) |
| `dnats.count` | Number of DNAT configs | `.len()` (bare usize) |
| `daemon` | Daemon name label | `"natmap"` |
| `socket.path` | Unix socket path | `%path` |
| `mapping` | Full mapping struct for debug | `?m` |
| `error` | Error description, full `wrap_err` chain | `%format!("{e:#}")` |
| `regex` | Regex pattern string | `%cr` |
| `program` | External program name | `%program` |
| `args` | Command-line arguments | `%args_str` |

### Why `error` uses `{e:#}` and not `%e`

`eyre::Report`'s `Display` prints **only the outermost context**. `%e` on a
wrapped error therefore logs `natmap command failed` and silently drops the
cause underneath it. Every failure mode that reaches one `wrap_err` arm then
produces the same log line, which makes the journal useless for diagnosis.

Use `%format!("{e:#}")` (Alternate `Display`, the `a: b: c` chain) for every
`error` field. For an unwrapped error it renders identically to `%e`, so there
is no cost to always using it. For a `String` or a type with no `Alternate`
impl it falls back to plain `Display`.

`docs/dev/standards.md` forbids `expect`/`unwrap` in library code; a log line
that hides its own cause is the same class of problem — an error path that
cannot be acted on.

## 3. Spans — `#[instrument]` and `.instrument()`

Every async function that handles a Docker event or applies an iptables rule MUST be wrapped in a span.

### `#[instrument]` rules

- Always `skip_all` on functions with large or opaque arguments (e.g. `EventMessage`, `AppState`, `Arc<_>`)
- Add meaningful fields explicitly via `fields(...)`
- Pre-declare fields you'll populate later with `tracing::field::Empty`

```rust
#[instrument(
    skip_all,
    fields(
        service.id_prefix = %resolved.service_id_prefix,
        host.port         = tracing::field::Empty,
    )
)]
async fn apply_service(&self, resolved: &ResolvedService) -> Result<()> {
    // ...
    Span::current().record("host.port", host_port);
    // ...
}
```

### Async span safety

Never use `span.enter()` across `.await` points. Use `.instrument(span)` on futures instead:

```rust
async move { /* loop body */ }
    .instrument(info_span!("event_loop", daemon = "natmap"))
    .await;
```

## 4. Span hierarchy

### natmap

```
daemon                  ← long-lived, fields: daemon="natmap", socket.path
  docker_maintenance    ← long-lived, field: daemon="natmap"; reconnect, reload, event resubscribe
  handle_docker_event     ← per-event, fields: container.id, event.action
  reload                  ← fields: mappings.count, dnats.count
  add_dnat / remove_dnat  ← fields: ext.ip, int.ip, ports, proto
  add_mapping             ← fields: host.addr, container.addr, proto, container.id
  apply_service           ← fields: service.id_prefix, host.port, container.port
```

## 5. Log level semantics

| Level | Use for |
|-------|---------|
| `error!` | Unrecoverable operation failure requiring operator attention. Iptables apply failed. Daemon socket bind failed. |
| `warn!` | Self-recovered failure or degraded state. Retry succeeded. Container no longer exists when processing its event. Hairpin NAT failed (non-fatal). Port conflict on crash recovery (skipped with warning). |
| `info!` | Low-frequency state transitions an operator cares about: daemon start/stop, rule added/removed, service registered/deregistered, sync complete. ONE line per user-visible operation, NOT per Docker event. |
| `debug!` | Per-event detail and control-plane internals: what rule was computed, which target matched which service, which event was filtered and why. |
| `trace!` | Raw payloads: full `EventMessage` struct, raw iptables command, intermediate computed values. `trace!(?raw_event, "raw docker event")` |

The Docker event stream is high-volume. **Every raw Docker event MUST be logged at `debug!` or lower, never `info!`.**

## 6. Subscriber initialization

The subscriber is initialized once in the root `src/main.rs`. Workspace crates never initialize tracing. Use `RUST_LOG` env var for filtering.
