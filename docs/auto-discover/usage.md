## Terms

- **Proxy server**: Public-facing reverse proxy server that routes traffic to other servers (currently `proxy-node-1`)
- **Service server**: Servers that host Docker services
- **Service**: A named group of containers. e.g., `example-drive`
- **auto-discover**: Rust daemon that watches Docker events, manages port forwarding via `lab-ops`, registers services with Consul, generates nginx configs stored in Consul KV, and syncs forwarding/nginx rules on the proxy server. Components are controlled with `--no-discovery`/`--no-forwarding`/`--no-nginx` flags
- **lab-ops natmap**: Manages iptables NAT rules, including dynamic Docker port mappings
- **Forwarding**: Kernel-level NAT (iptables DNAT) that bypasses NGINX reverse proxy for latency-sensitive or non-HTTP services (e.g., game servers, mail servers). Managed via `lab-ops natmap dnat` on the proxy server
- **auto-discover nginx component**: Runs as part of the unified daemon on the proxy server, watches Consul KV for nginx config changes, applies post-processing, and writes per-service configs to `sites-available/` and `streams-available/`. nginx-ui manages `sites-enabled/` symlinks. Disable with `--no-nginx`
- **Static nginx configs**: Proxy-local services (Consul UI, NGINX-UI) served directly from the proxy server via static nginx configs at `/etc/nginx/sites-available/`. These do NOT go through Docker/Consul/auto-discover

## Architecture

The cluster uses two networks: Tailscale (`100.64.0.x` CGNAT) for user-facing access, and a VM bridge (`10.0.0.x`) for Consul gossip and inter-service traffic. The proxy server (`proxy-node-1`) runs the auto-discover daemon (`--no-discovery`) to route traffic from the public IP (`203.0.113.43`) and Tailscale IP to service VMs.

Proxy-local services (Consul UI, NGINX-UI) are served by static nginx configs — they don't go through Docker/Consul/auto-discover.

```
Service Server                           Proxy Server
─────────────                           ─────────────
Docker Container:80                     lab-ops auto-discover daemon
    │                                      │  (--no-discovery)
    ▼                                      │  watches Consul KV
lab-ops natmap docker add                  │  blocking queries
    │  (iptables DNAT)                     │
    ▼                                      ▼
10.0.0.101:32000 ←──────────────  NGINX configs written:
    │                                /etc/nginx/sites-available/{id}.conf
    ▼                                /etc/nginx/streams-available/{id}.conf
auto-discover (daemon)                     │
    │  generates nginx config        nginx-ui manages sites-enabled
    │  runs generator script         symlinks for enable/disable
    │  stores in Consul KV:               │
    │  nginx-configs/sites/{id}.conf      ▼
    │  registers to Consul:         NGINX reverse proxy (http + stream)
    │  - Address: 10.0.0.101            │  reloaded on config change
    │  - Port: 32000                      │
    │  - Meta.proxy_ip: 203.0.113.43      │
    │  - Meta.template: HTTP              ▼
    │  - Meta.domain: drive.example.com  Internet ← 203.0.113.43:80/443
    ▼
Consul Agent ──────────────────────────→ Consul Server
                                      (proxy-node-1)
                                        │
                                        ▼
                                   forwarding component
                                      │  polls Consul every 30s
                                      │  for Meta.forwarding=="true"
                                      ▼
                                   lab-ops natmap dnat
                                      │  iptables DNAT + hairpin
                                      ▼
                                   Internet ← 203.0.113.43:<ext_port>
```

**Nginx config generation**:
- Service nodes: `lab-ops auto-discover daemon` calls `/usr/local/bin/auto-discover-gen-nginx` with `AUTO_DISCOVER_*` env vars, applies inline `preprocess`, and stores the result in Consul KV at `nginx-configs/{sites,streams}/{service_id}.conf`
- If `postprocess` is configured, the script content is stored alongside at `nginx-configs/{sites,streams}/{service_id}.postproc`
- Proxy server: `lab-ops auto-discover daemon --no-discovery` watches Consul KV with blocking queries, pipes each config through per-service postproc scripts + common postprocs from `/etc/auto-discover/postprocs.d/`, and writes to `/var/lib/auto-discover/nginx-configs/`
- Configs are symlinked to `/etc/nginx/sites-available/` or `/etc/nginx/streams-available/`
- nginx-ui manages `sites-enabled/` and `streams-enabled/` symlinks for enable/disable
- Adding or changing a service triggers Consul KV update → automatic nginx regeneration

### Route flow
1. Internet → Proxy Server (NGINX) → Service Server VM IP:port → iptables DNAT → Docker container
2. Internet → Service Server (public) → Service (non-proxy path)
3. Internet → Proxy Server (kernel DNAT) → Service Server (direct NAT forwarding, no NGINX)

### Forwarding Architecture (kernel-level NAT)

For services with `forwardlocal` or `forwardremote` config, the flow bypasses NGINX entirely:

```
Service Server                           Proxy Server
─────────────                           ─────────────
Docker Container:25565                  lab-ops auto-discover daemon
    │                                         │  (--no-discovery --no-nginx)
    ▼                                         │
lab-ops natmap docker add                     │ (reads Consul forwarding meta)
    │  (iptables DNAT, static port)           │
    ▼                                         ▼
10.0.0.102:25565                    lab-ops natmap dnat
    │                                   (PREROUTING + FORWARD rules)
    │                                         │
    ▼                                         ▼
lab-ops auto-discover (daemon)         iptables DNAT:
    │  registers to Consul:             ext_ip:25565 → 10.0.0.102:25565
    │  - Meta.forwarding: true                │
    │  - Meta.ext_ip: 203.0.113.43     (optionally hairpin NAT for
    │  - Meta.ext_ports: 25565          internal access via external IP)
    │  - Meta.hairpin: true                   │
    ▼                                         │
Consul Agent ──────────────────────────→ Consul Server
                                  (proxy-node-1)
```

The proxy server runs `lab-ops auto-discover daemon --no-discovery --no-nginx` via systemd. See [[#forwarding-daemon]] for the service unit and polling details.

## Configuration

### Node Discovery Config (`/etc/auto-discover/discovery.yaml`)

Each service server has a single YAML file at `/etc/auto-discover/discovery.yaml` that defines all services running on that node.

```yaml
# /etc/auto-discover/discovery.yaml

node:
  name: service-node-1        # node identity (used for Consul service IDs and stale cleanup)

defaults:
  proxy_ip: 203.0.113.43      # cascades to each service entry
  proxy_on: proxy-node-1       # proxy server node name (optional, for multi-proxy)
  bind_ip: 10.0.0.101         # cascades: per-service → defaults → container IP (fallback)
  bind_interface: eth0         # resolved via `ip -j -4 addr show <iface>`
  nginx_generator: /usr/local/bin/auto-discover-gen-nginx  # path to generator script
  preprocess: ""               # default preprocess script (runs on service node)
  postprocess: ""              # default postprocess script (runs on proxy)

services:
  example-drive:
    type: docker               # "docker" or "local"
    match:
      project: example-drive   # must match com.docker.compose.project label
    rproxylocal:               # reverse proxy entries for services on this node (nginx configs)
      - port: 80
        template: HTTP_PROXY
        domains:
          - drive.example.com

  example-mail:
    type: docker
    match:
      project: example-mail
    extra:
      eas: "true"
    bind_ip: 10.0.0.101        # overrides defaults.bind_ip
    rproxylocal:
      - port: 80
        template: HTTP_PROXY
        domains:
          - mail.internal.example.com

  example-mc:                  # same project, multiple ports
    type: docker
    match:
      project: example-mc
    rproxylocal:
      - port: 25565            # TCP entry
        template: TCP_PROXY
      - port: 19132            # UDP entry
        template: TCP_PROXY
    forwardremote:             # kernel-level NAT on proxy server (bypasses NGINX)
      - port: 25565
        ext_ip: 203.0.113.43
        ext_ports: [25565]
        proto: tcp
        hairpin: true
```

**Top-level fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `node` | Yes | Node identity (see below) |
| `config_dir` | No | Directory for generated configs. Default: none |
| `defaults` | No | Cascade defaults for all services (see below) |
| `services` | Yes | Map of service definitions (key = service name, used as Consul service name) |

**`node` fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `name` | Yes | Node identity. Used for Consul service ID prefix and stale-service cleanup |

**Per-service fields (`services.<name>`):**

| Field | Required | Description |
|-------|----------|-------------|
| `type` | Yes | `docker` (matches Docker containers) or `local` (runs directly on host) |
| `match` | Yes (docker) | Container matching rules. Required for `type: docker` to avoid matching unrelated containers. See [[#Match Config]] below |
| `address` | No | IP address for `type: local` services. Not used for Docker services |
| `bind_ip` | No | IP to bind the natmap host port on. Cascades from `defaults.bind_ip`. Falls back to container Docker IP |
| `bind_interface` | No | Interface name to resolve an IP from via `ip -j -4 addr show`. Cascades from `defaults.bind_interface` |
| `rproxylocal` | No | List of reverse proxy port entries for services on this node. Each entry generates an nginx config stored in Consul KV. See [[#RProxyLocal Config]] below |
| `rproxyremote` | No | List of reverse proxy port entries for services on other nodes. Requires `proxy_on` to specify target proxy. See [[#RProxyRemote Config]] below |
| `forwardlocal` | No | List of kernel-level NAT port entries for iptables DNAT on this node. See [[#ForwardLocal Config]] below |
| `forwardremote` | No | List of kernel-level NAT port entries for iptables DNAT on the proxy server. See [[#ForwardRemote Config]] below |
| `extra` | No | Arbitrary key-value pairs passed to the generator script as `AUTO_DISCOVER_EXTRA_<key>` env vars |

**Match Config (`services.<name>.match`):**

| Field | Required | Description |
|-------|----------|-------------|
| `project` | No | Only match containers with this `com.docker.compose.project` label |
| `container` | No | Only match a container with this exact name |
| `container_regex` | No | Only match containers whose name matches this regex |

At least one match field should be set. If `match` is absent, the service matches **any** container exposing the configured port — use with caution.

**RProxyLocal Config (`services.<name>.rproxylocal[]`):**

| Field | Required | Description |
|-------|----------|-------------|
| `port` | Yes | Container/host port the service listens on |
| `template` | Yes | Nginx template type (e.g., `HTTP_PROXY`, `TCP_PROXY`). Used by your custom nginx generator script. |
| `domains` | No | Domain names for NGINX `server_name`. First domain is the primary — also used as a discriminator in the Consul service ID to prevent collisions when multiple entries share the same name+port |
| `proxy_ip` | No | Override for the proxy server IP. Cascades from `defaults.proxy_ip` |
| `nginx_generator` | No | Path to nginx config generator script. Cascades from `defaults.nginx_generator`. Default: `/usr/local/bin/auto-discover-gen-nginx` |
| `preprocess` | No | Inline shell script run on the service node after the generator. stdin = generator output, stdout = stored config. Cascades from `defaults.preprocess` |
| `postprocess` | No | Inline shell script stored in Consul KV, run on the proxy. stdin = config from KV, stdout = final nginx config. Exit 1 = skip. Cascades from `defaults.postprocess` |

**RProxyRemote Config (`services.<name>.rproxyremote[]`):**

| Field | Required | Description |
|-------|----------|-------------|
| `port` | Yes | Container/host port the service listens on |
| `template` | Yes | Nginx template type (e.g., `HTTP_PROXY`, `TCP_PROXY`). Used by your custom nginx generator script. |
| `domains` | No | Domain names for NGINX `server_name`. First domain is the primary — also used as a discriminator in the Consul service ID to prevent collisions when multiple entries share the same name+port |
| `proxy_on` | Yes | Target proxy server node name where this nginx config should be generated |
| `proxy_ip` | No | Override for the proxy server IP. Cascades from `defaults.proxy_ip` |
| `nginx_generator` | No | Path to nginx config generator script. Cascades from `defaults.nginx_generator`. Default: `/usr/local/bin/auto-discover-gen-nginx` |
| `preprocess` | No | Inline shell script run on the service node after the generator. stdin = generator output, stdout = stored config. Cascades from `defaults.preprocess` |
| `postprocess` | No | Inline shell script stored in Consul KV, run on the proxy. stdin = config from KV, stdout = final nginx config. Exit 1 = skip. Cascades from `defaults.postprocess` |

**Defaults fields:**

| Field | Description |
|-------|-------------|
| `proxy_on` | Default proxy server node name for all services |
| `proxy_ip` | Default proxy server listen IP for all services |
| `bind_ip` | Default natmap bind IP for all services |
| `bind_interface` | Default interface for IP resolution |
| `nginx_generator` | Default path to nginx config generator script |
| `preprocess` | Default preprocess script |
| `postprocess` | Default postprocess script |

**Bind IP resolution order (per service):**
1. `services.<name>.bind_ip` (explicit IP)
2. `services.<name>.bind_interface` → resolved via `ip -j -4 addr show`
3. `defaults.bind_ip`
4. `defaults.bind_interface` → resolved
5. Container's Docker network IP (fallback)

**ForwardLocal Config (`services.<name>.forwardlocal[]`):**

Each entry in `services.<name>.forwardlocal[]` defines a kernel-level NAT port that creates the iptables DNAT rule directly on the service node via `lab-ops natmap`. No proxy-server forwarding sync needed. ForwardLocal bypasses NGINX entirely — it creates iptables DNAT rules to route traffic directly from an external IP to the service, eliminating proxy latency for game servers, mail servers, etc.

| Field | Required | Description |
|-------|----------|-------------|
| `port` | Yes | Container/host port to forward |
| `proto` | No | Protocol for the iptables DNAT rule. Defaults to `tcp` |
| `bind_ip` | No | Override for the natmap bind IP on this forwarding entry. Cascades from service-level `bind_ip` |
| `bind_interface` | No | Override for the interface on this forwarding entry. Cascades from service-level `bind_interface` |
| `bind_port` | No | Static host port. When set, uses this port directly (no ephemeral allocation). When absent, allocates from the ephemeral pool |

**ForwardRemote Config (`services.<name>.forwardremote[]`):**

Each entry in `services.<name>.forwardremote[]` defines a kernel-level NAT port that registers Consul metadata (`forwarding=true`, `ext_ip`, `ext_ports`, `hairpin`) for the proxy server's forwarding daemon to sync DNAT rules. The proxy server handles the iptables DNAT from a public IP to the service node address. ForwardRemote bypasses NGINX entirely — it creates iptables DNAT rules to route traffic directly from an external IP to the service, eliminating proxy latency for game servers, mail servers, etc.

| Field | Required | Description |
|-------|----------|-------------|
| `port` | Yes | Container/host port to forward |
| `proto` | No | Protocol for the iptables DNAT rule. Defaults to `tcp` |
| `ext_ip` | Yes | Public IP on the proxy server to forward FROM |
| `ext_ports` | Yes | Static port(s) on the public IP (not auto-allocated from ephemeral range). First port (`ext_ports[0]`) is used as the Consul registration port. No local natmap mapping or port check occurs on the service node |
| `hairpin` | No | Create hairpin NAT rules (internal hosts can reach themselves via external IP). Defaults to `false` |
| `proxy_on` | No | Only apply DNAT rules on this specific proxy server node name |

**Examples:**

ForwardRemote (proxy-server DNAT):
```yaml
services:
  example-mc:
    type: docker
    match:
      project: example-mc
    forwardremote:
      - port: 25565
        ext_ip: 203.0.113.43
        ext_ports: [25565]
        proto: tcp
        hairpin: true
```

ForwardLocal (direct node DNAT):
```yaml
services:
  example-mc:
    type: docker
    match:
      project: example-mc
    forwardlocal:
      - port: 25565
        bind_port: 36000
```

**How it works (ForwardRemote):**

1. **Service server**: The daemon uses `ext_ports[0]` as the Consul registration host port value. No local natmap mapping is created — the DNAT rule lives entirely on the proxy server. The port is NOT persisted to `ports.json`. No `port_is_free` check is performed on the service node.
2. **Service server**: Registers in Consul with forwarding meta (`forwarding=true`, `forwarding_type=remote`, `ext_ip`, `ext_ports`, `hairpin`)
3. **Proxy server**: Runs `lab-ops auto-discover daemon --no-discovery --no-nginx` (or one-shot `forwarding-sync`), which:
   - Queries Consul **catalog** API (`GET /v1/catalog/services` → `GET /v1/health/service/:name?passing=true`) across all agents — NOT the local agent API. Forwarding services are registered on service VMs' agents, not the proxy's agent
   - Filters services with `Meta.forwarding=="true"`
   - Groups by `(ext_ip, address, protocol)`
   - Removes stale DNAT rules (any existing rules with no matching Consul entry)
   - Applies DNAT rules via `IptablesManager`
   - Optionally applies hairpin rules for hairpin-enabled groups (**non-fatal**: if hairpin fails, the forwarding sync continues and logs a warning)
   - Handles deregistration of stale DNAT rules

> **Note**: The forwarding daemon's `remove_dnat` and `remove_hairpin` loop `iptables -D` until all copies of a rule are removed. This prevents duplicate rules from accumulating if multiple sync cycles run before a group's hairpin succeeds.

**How it works (ForwardLocal):**

1. **Service node**: The daemon uses `bind_port` as a static host port (or allocates from ephemeral pool if unset). Always calls natmap to create the DNAT rule on the service node.
2. **Service node**: Registers in Consul with `forwarding=true`, `forwarding_type=local`. No `ext_ip`, `ext_ports`, or `hairpin` metadata.
3. **No proxy-server DNAT sync**: ForwardLocal does NOT participate in the proxy-server forwarding daemon. DNAT is local to the service node.

### Proxy Server NGINX Config Generation

The proxy server runs **`lab-ops auto-discover daemon --no-discovery`** as a systemd daemon that watches Consul KV for nginx config changes using Consul's blocking-query mechanism.

**Flow:**
1. Service nodes generate nginx configs via `/usr/local/bin/auto-discover-gen-nginx` and store them in Consul KV at `nginx-configs/sites/{service_id}.conf` (or `streams/`)
2. The auto-discover daemon watches the `nginx-configs/` KV prefix. When any key changes, it:
   - Reads all `.conf` keys
   - Pipes each through the service's postproc script (if stored at `.postproc` key)
   - Runs all common postprocs from `/etc/auto-discover/postprocs.d/` in lexicographic order
   - Writes processed configs to `/var/lib/auto-discover/nginx-configs/`
   - Symlinks to `/etc/nginx/sites-available/` or `/etc/nginx/streams-available/`
   - Runs `nginx -t && systemctl reload nginx` if configs changed
3. nginx-ui manages `sites-enabled/` and `streams-enabled/` symlinks — the daemon never touches them

**Stale config GC:** Every 5 minutes, the daemon performs a GC sweep that cross-references the KV entries against the Consul catalog. If a KV entry's service ID no longer exists in any registered Consul service (e.g., a node crashed and `DeregisterCriticalServiceAfter` auto-removed the service after 5 minutes), the orphaned config and postproc KV entries are deleted. This prevents stale configs from accumulating and disrupting nginx with references to dead upstream addresses.

**Generator script** (`/usr/local/bin/auto-discover-gen-nginx`):
- Receives service data via `AUTO_DISCOVER_*` env vars
- Outputs raw nginx config to stdout
- Optionally piped through `preprocess` (inline shell in discovery.yaml) before storage

**Common postprocs** (`/etc/auto-discover/postprocs.d/`):
- `10-handle-tailscale-private`: substitutes `__TAILSCALE_IP__` → actual Tailscale IP. Exits 1 (skip service) if tailscale is unreachable and config contains the placeholder

**auto-discover-nginx systemd unit** (proxy server, nginx component only):

```ini
[Unit]
Description=auto-discover — NGINX config generator
Requires=consul.service network-online.target
After=consul.service network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/lab-ops auto-discover daemon --no-discovery --no-forwarding
Restart=on-failure
RestartSec=10
Environment=TAILSCALE_IP=<tailscale-ip>
Environment=TAILSCALE_REACHABLE=true|false
```

**Dynamic updates**: the auto-discover daemon uses Consul KV blocking queries (long-polling with an index parameter). Any KV change under `nginx-configs/` triggers regeneration and reload.

### forwarding-daemon

The proxy server runs `lab-ops auto-discover daemon --no-discovery --no-nginx` as a systemd daemon. It polls Consul every 30s for services with `Meta.forwarding=="true"` and applies `lab-ops natmap dnat` rules. Static ports are configured in `discovery.yaml` — no ephemeral allocation.

**systemd unit** (proxy server, forwarding component only):

```ini
[Unit]
Description=Lab Discovery Forwarding Daemon
Requires=consul.service network-online.target
After=consul.service network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/lab-ops auto-discover daemon --no-discovery --no-nginx
Restart=on-failure
RestartSec=10
```

## Consul Service Registration

`auto-discover` registers each service instance to the local Consul agent with this structure:

```json
{
  "ID": "service-node-1-drive-example-com-32000",
  "Name": "example-drive",
  "Address": "10.0.0.101",
  "Port": 32000,
  "Meta": {
    "domain": "drive.example.com",
    "template": "HTTP_PROXY",
    "protocol": "tcp",
    "proxy_ip": "203.0.113.43",
    "server_name": "service-node-1",
    "generation_id": "service-node-1-a1b2c3d4e5f6g7h8",
    "container_id": "abc123def456",
    "client_max_body_size": "50M"
  },
  "Check": {
    "TCP": "10.0.0.101:32000",
    "Interval": "30s",
    "Timeout": "10s",
    "DeregisterCriticalServiceAfter": "5m"
  }
}
```

When forwarding is configured, additional meta fields are present:

**ForwardRemote** (proxy-server DNAT):
```json
{
  "Meta": {
    "forwarding": "true",
    "forwarding_type": "remote",
    "ext_ip": "203.0.113.43",
    "ext_ports": "25565",
    "hairpin": "true"
  }
}
```

**ForwardLocal** (service-node DNAT):
```json
{
  "Meta": {
    "forwarding": "true",
    "forwarding_type": "local"
  }
}
```

**Fields:**

- `ID`: `{server_name}-{domain_slug}-{host_port}`. Dots in domain replaced with dashes. Falls back to `{server_name}-{service_name}-{host_port}` when no domain is configured
- `Name`: Service name from `discovery.yaml`
- `Address`: `bind_ip` (where NGINX proxies to)
- `Port`: Allocated host port (via `lab-ops natmap`)
- `Meta.domain`: Primary domain for NGINX `server_name`
- `Meta.template`: Template file name on the proxy server
- `Meta.protocol`: `tcp` or `udp`
- `Meta.proxy_ip`: Proxy server IP (used by generator script `listen` directive)
- `Meta.proxy_on`: Target proxy server node name for nginx config generation (RProxyRemote only)
- `Meta.generation_id`: Deterministic config version for stale service cleanup (`{node_name}-{sha256_of_config[:16]}`)
- `Meta.container_id`: Docker container ID for per-container deregistration
- `Meta.*`: Any `extra` fields from `discovery.yaml` are passed through as-is

**Forwarding meta fields (only when `forwarding` is configured):**

- `Meta.forwarding`: `"true"` — marker for proxy server to discover forwarding services
- `Meta.forwarding_type`: `"remote"` or `"local"` — distinguishes proxy-server DNAT from service-node DNAT
- `Meta.ext_ip`: Public IP on the proxy server for DNAT (ForwardRemote only)
- `Meta.ext_ports`: Comma-separated static ports (e.g., `"25565,19132"`) (ForwardRemote only)
- `Meta.hairpin`: `"true"` if hairpin NAT is requested (ForwardRemote only)

### UDP Checks

UDP services use a `netcat`-based health check instead of TCP:

```json
{
  "Check": {
    "Name": "UDP check for example-mc",
    "Args": ["/usr/bin/nc", "-uz", "10.0.0.102", "32769"],
    "Interval": "30s",
    "Timeout": "10s",
    "DeregisterCriticalServiceAfter": "5m"
  }
}
```

### Nginx Config KV Query

The auto-discover daemon watches Consul KV with blocking queries:

```
GET /v1/kv/nginx-configs/?recurse=true&wait=55s&index=X
```

Returns all `.conf` and `.postproc` keys. The daemon processes each config through per-service and common postprocs, writes to disk, and reloads nginx on change.

## auto-discover Daemon

### Container Matching

The daemon matches Docker containers to service definitions using the match config criteria (`project`, `container`, `container_regex`).

For `sync()`, containers are matched via the match config criteria. Docker events (`handle_container_start`) trigger the same matching logic.

### Operations

1. **On startup**: Parse `/etc/auto-discover/discovery.yaml`. Sync all running Docker containers matching configured services via the two-level filter above. The initial sync retries up to 10 times with exponential backoff (2s → 30s) in case `natmap.service` socket is not yet ready — this prevents the race condition where `lab-ops auto-discover` starts before natmap creates `/run/natmap.sock`. Partial failures (one service failing) do not abort the sync — errors are logged and the remaining services are processed.

2. **On Docker event (start)**:
   - Match container to all services in `discovery.yaml` where `services.<name>.match.project == compose_project`
   - Determine bind IP via the resolution chain (service bind_ip → bind_interface → defaults → container IP)
   - **ForwardRemote**: Use `ext_ports[0]` as the Consul registration port. No local natmap mapping or port check is performed.
   - **ForwardLocal (with `bind_port`)**: Use the static `bind_port` directly. Call natmap to create the local DNAT rule.
   - **ForwardLocal (no `bind_port`)**: Allocate an ephemeral port from the pool. Call natmap to create the local DNAT rule.
   - **Non-forwarding service**: Allocate a persistent free host port from the ephemeral range (32768-60999). Call natmap to create the Docker mapping.
   - Register the service to Consul with all metadata (including forwarding meta when applicable)

3. **On Docker event (die)**:
   - Deregister all matching Consul services by `container_id`
   - natmap rules are cleaned up by the natmap daemon's container event watcher

4. **On config file change**: Re-parse `discovery.yaml` and sync. Stale services from previous config generations are automatically deregistered.

### Systemd Service

Deployed as `auto-discover.service` (service node, discovery only):

```
[Unit]
Description=Lab Discovery Daemon
Requires=docker.service natmap.service consul.service
After=docker.service natmap.service consul.service network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/lab-ops auto-discover daemon --no-forwarding --no-nginx
Restart=on-failure
RestartSec=10

[Install]
WantedBy=multi-user.target
```

### CLI Interface

```bash
# Run unified daemon (all components enabled by default)
lab-ops auto-discover daemon

# Run discovery only (service node)
lab-ops auto-discover daemon --no-forwarding --no-nginx

# Run forwarding + nginx only (proxy server)
lab-ops auto-discover daemon --no-discovery

# Run a single sync pass and exit
lab-ops auto-discover sync

# Validate config without running
lab-ops auto-discover check

# Run on proxy server: one-shot sync of DNAT rules from Consul
lab-ops auto-discover forwarding-sync [--consul-addr http://127.0.0.1:8500]

# Run on proxy server: one-shot sync of nginx configs from Consul KV
lab-ops auto-discover nginx-sync [--consul-addr http://127.0.0.1:8500]

# Show version
lab-ops auto-discover --version
```

**Startup retry**: The `daemon` subcommand retries the initial discovery sync up to 10 times with exponential backoff (2s → 30s max). This handles the race condition where `natmap.service` has not created `/run/natmap.sock` yet when `auto-discover.service` starts. If all retries fail, the daemon continues running and will catch up via Docker container `start` events.

## Port Management

Ports are allocated from the range 32768-60999 and persisted to `/var/lib/auto-discover/ports.json`. The port mapping is managed by `lab-ops natmap docker add/rm` which handles the iptables rules.

**ForwardRemote services** use static ports from `ext_ports[0]` instead of ephemeral allocation. These ports are NOT persisted to `ports.json` (they're static, not from the pool). The `port_is_free` check still verifies no other process holds the port before assigning it.

## Generation Tracking

Each configuration deployment generates a `generation_id` (`{node_name}-{sha256_of_discovery_yaml[:16]}`). The node name is taken from `node.name` in `discovery.yaml`. This allows cleanup of stale Consul registrations from previous deployments and ensures per-node isolation.

## Node Identity

The `node.name` field in `discovery.yaml` is the single source of node identity. It replaces:
- `hostname::get()` (unreliable across environments)
- `server.json` `name` field (was never wired up, now removed)
- `server.json` `pass_ip` → now `defaults.bind_ip`
- `server.json` `proxy_ip` → now `defaults.proxy_ip`

## Binary Deployment

1. **Build** `lab-ops` binary via `cargo build --release`
2. **Copy** binary from `target/release/lab-ops` to `/usr/local/bin/lab-ops`
3. **Create** `/etc/auto-discover/` and `/var/lib/auto-discover/` directories
4. **Deploy** `discovery.yaml` to `/etc/auto-discover/discovery.yaml`
5. **Deploy** `auto-discover.service` systemd unit (daemon mode)
6. **Depends on**: `consul.service` + `natmap.service` (from `lab-ops natmap install`)

Proxy-local static nginx configs at `/etc/nginx/sites-available/consul` and `/etc/nginx/sites-available/web` serve the Consul web UI (`127.0.0.1:8500`) and NGINX-UI (`100.64.0.1:9000`) on the proxy server itself.
