# lab-ops

Homelab tooling for NAT and service discovery across Docker hosts. The natmap daemon owns iptables NAT rules; auto-discover turns container and service events into NAT rules plus Consul registrations.

## Language

### Service exposure

**Forwarding**:
How auto-discover exposes a service on an external address. A forwarding rule carries: external IP, internal IP, ports, protocol, hairpin, and preserve_src_ip.
_Avoid_: reverse proxy, port redirect

**Forwarding sync**:
The process that aligns the iptables forwarding rules with the set of services registered in Consul — creating, updating, and deleting rules so live rules match desired rules.

**preserve_src_ip**:
The intent that a client's original source IP remains visible to the destination. Realized by omitting MASQUERADE on the rule, plus optional policy routing and a hairpin MASQUERADE limited to the LAN CIDR.
_Avoid_: no_masquerade, source preservation

**Hairpin**:
The ability to reach a forwarded service through its external address from inside the LAN. Requires a MASQUERADE so the reply leaves via the LAN gateway.

### Routing

**NAT rule**:
A persistent rule config the natmap daemon manages in iptables. Kinds: port mapping, static DNAT, SNAT, hairpin, policy route.

**Port mapping**:
An association from host_ip:host_port to container_ip:container_port for a running container.
_Avoid_: mapping, docker mapping

**Live rule**:
A rule currently present in iptables, as distinct from the daemon's persisted state.
_Avoid_: rule state, state

### Consistency

**Reconcile**:
Bringing live state (iptables, Docker, Consul) back in line with desired state. The daemon's reload path and auto-discover's forwarding sync are both reconciles.

**Natmap daemon**:
The central authority for the NAT rules it creates; persists them to state and reinstalls on reload.
