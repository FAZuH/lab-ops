# lab-ops

Homelab tooling for NAT across Docker hosts. The natmap daemon owns iptables NAT rules; a node's `discovery.yaml` declares the ports its services own, and `natmap apply` converges the daemon to that declaration.

## Language

### Service exposure

**Discovery config**:
A `discovery.yaml` file declaring a node's services: identity, defaults, and per-service match, port ownership and port registrations. Parsed and resolved by `lab_ops_lab_lib::discovery`, which applies defaults and expands each service into one resolved registration per port.
_Avoid_: service file, inventory

**Declaration**:
The part of a discovery config that natmap converges to: each service's owned ports (`ports:`) and the address each port answers on. One declared port produces at most one mapping, on the declared port — never an allocated one.
_Avoid_: desired state, manifest

**Forwarding**:
How a service is exposed on an external address. A forwarding rule carries: external IP, internal IP, ports, protocol, hairpin, and preserve_src_ip.
_Avoid_: reverse proxy, port redirect

**Apply**:
Bringing a node's mappings in line with its declaration: diff them, then create, update and release. Written by `natmap plan` (report) and `natmap apply` (execute). Also names the older path that maps one already-resolved service (`apply_service`).
_Avoid_: forwarding sync, reconcile

**Ownership ledger**:
The service each mapping records as owning its port. An apply refuses to hand a port to any other service, and releases a port only in the apply that removes its owner.
_Avoid_: port registry, allocation table

**Covered port**:
A declared port that needs no mapping because the service already answers on the node's tailnet address, or on every address.
_Avoid_: skipped port, no-op mapping

**Drift**:
Anything the daemon holds that disagrees with the declaration. `plan` and `apply --dry-run` report it and exit non-zero.
_Avoid_: difference, delta

**preserve_src_ip**:
The intent that a client's original source IP remains visible to the destination. Realized by omitting MASQUERADE on the rule, plus optional policy routing and a hairpin MASQUERADE limited to the LAN CIDR.
_Avoid_: no_masquerade, source preservation

**Hairpin**:
The ability to reach a forwarded service through its external address from inside the LAN. Requires a MASQUERADE so the reply leaves via the LAN gateway.

### Routing

**NAT rule**:
A persistent rule config the natmap daemon manages in iptables. Kinds: port mapping, static DNAT, SNAT, hairpin, policy route.

**Port mapping**:
An association from host_ip:host_port to container_ip:container_port for a running container, or to a local service address. A mapping an apply created records its owner; a hand-added mapping records none.
_Avoid_: mapping, docker mapping

**Locally delivered destination**:
A DNAT destination that is an address on this host (`127.0.0.1`, `::1`). The DNATed packet is delivered locally, so it traverses the `input` hook, never `forward`, and the kernel rule alone services the mapping — the daemon holds no userspace port reservation for it.
_Avoid_: loopback forward

**Live rule**:
A rule currently present in iptables, as distinct from the daemon's persisted state.
_Avoid_: rule state, state

### Consistency

**Reconcile**:
Bringing live state (iptables, Docker) back in line with desired state. The daemon's reload path is the reconcile.

**Natmap daemon**:
The central authority for the NAT rules it creates; persists them to state and reinstalls on reload.
