# iptables Chain Layout

See [../diagrams/iptables-chains.png](../diagrams/iptables-chains.png) for the visual layout.

## Chain Overview

The natmap daemon sets up custom chains to isolate its rules from Docker's native iptables management:

### `filter` table

```
FORWARD
  └─→ DOCKER-USER (Docker's chain, always present)
        └─→ NATMAP ← natmap's jump rule (all natmap FORWARD rules go here)
```

- **DOCKER-USER**: Created by Docker. Rules here take priority over Docker's own rules.
- **NATMAP**: Created by natmap. Contains all natmap's FORWARD ACCEPT rules.

### `nat` table

```
PREROUTING
  └─→ NATMAP ← natmap's jump rule (all natmap DNAT rules go here)

OUTPUT
  └─→ DNAT rules for localhost traffic (direct)

POSTROUTING
  └─→ MASQUERADE rules (hairpin NAT)
```

- **NATMAP** (nat table): Contains natmap's DNAT rules for forwarded traffic.
- **OUTPUT**: natmap adds DNAT rules here for locally-generated traffic (e.g., `curl localhost`).
- **POSTROUTING**: Contains hairpin NAT MASQUERADE rules.

## Rule Types by Chain

### NATMAP (nat table)
- Docker port mapping DNAT rules (`-p tcp --dport 8080 -j DNAT --to-destination 172.17.0.2:80`)
- Static DNAT rules
- Hairpin NAT DNAT rules

### NATMAP (filter table)
- Docker port mapping FORWARD ACCEPT rules (`-d 172.17.0.2 -p tcp --dport 80 -j ACCEPT`)
- Static DNAT FORWARD ACCEPT rules

### PREROUTING (nat table)
- Static DNAT rules (directly in PREROUTING, not via NATMAP chain)
- Hairpin NAT DNAT rules (directly in PREROUTING)

### OUTPUT (nat table)
- Docker port mapping DNAT rules for localhost (`-d 127.0.0.1 -p tcp --dport 8080 -j DNAT`)
- Static DNAT OUTPUT rules (locally-generated traffic)

### POSTROUTING (nat table)
- Docker container MASQUERADE rules
- Static SNAT rules
- Hairpin NAT MASQUERADE rules

## Rule Comments

All Docker-related rules have an iptables comment for idempotent existence checks:
```
-m comment --comment "natmap:{container_id}:{host_port}"
```

This allows the daemon to find and delete specific rules by container ID and port.

## Crash Recovery Cleaning

On crash recovery (`reload_state()`), the daemon calls `iptables.flush_all_natmap()`:

1. `iptables -t nat -F NATMAP` — flush all rules from the nat NATMAP chain
2. `iptables -t nat -X NATMAP` — delete the nat NATMAP chain
3. `iptables -t filter -F NATMAP` — flush all rules from the filter NATMAP chain
4. `iptables -t filter -X NATMAP` — delete the filter NATMAP chain

After flushing, `iptables.setup()` is called to recreate the chains and jump rules, then rules are reinstalled from the state file.
