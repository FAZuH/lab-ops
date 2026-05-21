## Auto-Discover Config Refactor — Summary

### What Changed

1. **Config Schema** (`discovery.yaml`):
   - `name` → `node: { name: ... }` (top-level object for future extensibility)
   - `networks: [...]` → `services: { key: {...} }` (map with unique service keys)
   - Each service has `type: docker | local`, `match: { project?, container?, container_regex? }`, `rproxy: [...]`, `forwarding: [...]`
   - Service-level `address` replaces `local_ip` for local services
   - Local services bypass NAT entirely for reverse proxy
   - Backward compatibility: old format (`name` + `networks`) still works via automatic detection

2. **Container Matching** (`daemon.rs`):
   - New `container_matches()` handles `match.project`, `match.container`, `match.container_regex`
   - Old simple compose-project matching still works via `match.project` in auto-converted legacy configs
   - `handle_container_start()` uses the event's `compose_project` parameter for project matching

3. **Forwarding + RProxy Merging** (`config.rs`):
   - Forwarding entries merge with same-port RProxy entries to produce a single `ResolvedService`
   - `ForwardingRemote` and `ForwardingLocal` variants now carry template/domains for nginx config
   - Legacy forwarding-only entries (no template) skip nginx config

4. **Rust Structs** (`config.rs`):
   - New types: `NodeConfig`, `ServiceType`, `MatchConfig`, `RProxyConfig`, `ForwardingType`
   - `ForwardingConfig` restructured: `fwd_type`, `port`, optional fields
   - `ResolvedPortType` extended: `ForwardingLocal` and `ForwardingRemote` carry template info

5. **Consul Metadata** (`consul.rs`):
   - `build_consul_service` reads template from `ResolvedPortType` variants
   - `proxy_on` metadata added for future proxy server filtering

6. **Docker Integration** (`docker.rs`):
   - New `inspect_container()` returns `ContainerInfo` with name, compose project, exposed ports

### Test Results
- 11 auto-discover unit tests: all pass
- 29 lab-ops unit tests: all pass
- 44 auto-discover integration tests: all pass  
- 29 natmap integration tests: all pass
- Total: **113 tests, 0 failures**

### New Config Format Examples

```yaml
node:
  name: service-node-1

services:
  portainer:
    type: docker
    match:
      project: portainer
    rproxy:
      - port: 9000
        template: REVERSE_PROXY
        domains:
          - portainer.fazuh.com

  nginx-ui:
    type: local
    address: 100.64.0.1
    rproxy:
      - port: 9000
        template: REVERSE_PROXY_PRIVATE
        domains:
          - web.internal.fazuh.com

  example-mc:
    type: docker
    match:
      project: mc
    forwarding:
      - type: remote
        port: 25565
        ext_ip: 203.0.113.43
        ext_ports: [25565]
        proto: tcp
        hairpin: true
```
