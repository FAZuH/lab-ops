# Daemon Lifecycle

## Startup Sequence

See [../diagrams/daemon-startup.png](../diagrams/daemon-startup.png) for a visual flow.

1. **Docker Connection** — Attempts `docker::connect()`. If Docker socket is missing, logs a warning and continues without Docker support. This allows the daemon to run in environments without Docker (e.g., Proxmox host for static VM NATs).

2. **iptables Setup** — Ensures the `NATMAP` chain exists in both the `nat` and `filter` tables, with jump rules from `PREROUTING` and `DOCKER-USER`.

3. **Crash Recovery** (`reload_state()`):
   - **Flush stale rules:** Calls `iptables.flush_all_natmap()` to clear any leftover rules from a previous crashed instance
   - **Release ports:** Calls `ports.deallocate_all()` to drop all socket reservations
   - **Load state:** Reads `state.json`, or uses `DaemonState::default()` if missing
   - **Reconcile Docker:** For each persisted Docker mapping, checks if the container is still running. If dead, removes the mapping. If alive, rebinds the port and reinstalls iptables rules
   - **Reconcile static NATs:** For each DNAT/hairpin config, attempts to rebind ports. If a port is taken by another service, drops the config from state (with a warning) instead of hijacking
   - **Reconcile SNATs:** Reinstalls SNAT rules (no port binding needed)

4. **Docker Event Listener** — Spawns a tokio task listening for `container start` and `container die` events from the Docker daemon. On `start`, discovers port mappings and adds them. On `die`, flushes rules and removes mappings.

5. **Graceful Shutdown Handler** — Spawns a tokio task waiting for SIGTERM/SIGINT. On signal: flushes all NATMAP iptables rules, releases all port reservations, exits cleanly.

6. **API Server** — Binds to the Unix socket, sets permissions (`chown root:group`, `chmod 660`), and begins accepting connections in an infinite loop.

## Runtime Operations

### Adding a Rule (DNAT example)

1. CLI sends `POST /dnat` with JSON body
2. Daemon validates the request
3. For each port, calls `PortAllocator::allocate()` — binds `0.0.0.0:port`
4. If any port is taken, returns `409 Conflict` (no iptables rules created)
5. On success, calls `IptablesManager::install_dnat()` to create iptables rules
6. If iptables fails, releases the allocated ports (rollback)
7. Appends config to `DaemonState.dnats`
8. Calls `persist_state()` (atomic write to state.json)

### Removing a Rule

1. CLI sends `DELETE /dnat` with JSON body matching the config
2. Daemon finds the matching config in state
3. Calls `IptablesManager::remove_dnat()` to delete iptables rules
4. Calls `PortAllocator::deallocate()` for each port
5. Removes config from state
6. Calls `persist_state()`

## Crash Recovery

If the daemon process is killed (SIGKILL, OOM, power loss):

1. **Ports are released**: The OS automatically closes all sockets, freeing port reservations
2. **iptables rules persist**: Kernel iptables rules survive the process death
3. **On next startup**: The daemon flushes ALL stale rules, reads state.json, and only reinstalls rules whose ports can be successfully bound. Ports taken by other services (started while the daemon was dead) are skipped with a warning.

## State File Format

`/var/lib/natmap/state.json`:

```json
{
  "docker": {
    "abc123def456": [
      {
        "id": 1,
        "request": {
          "host_addr": "0.0.0.0:8080",
          "container_addr": "172.17.0.2:80",
          "proto": "tcp"
        },
        "container_id": "abc123def456",
        "container_name": "my-nginx",
        "rule_comment": "natmap:abc123def456:8080"
      }
    ]
  },
  "dnats": [
    {
      "ext_ip": "139.99.69.43",
      "int_ip": "10.10.10.101",
      "ports": "25,465",
      "proto": "tcp",
      "ext_if": "vmbr0"
    }
  ],
  "snats": [
    {
      "int_ip": "10.10.10.101",
      "ext_ip": "139.99.69.43",
      "ext_if": "vmbr0"
    }
  ],
  "hairpins": [
    {
      "ext_ip": "139.99.69.43",
      "int_ip": "10.10.10.101",
      "ports": "25,465",
      "proto": "tcp"
    }
  ]
}
```
