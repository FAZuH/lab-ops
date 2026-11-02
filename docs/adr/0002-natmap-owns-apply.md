# 0002 — natmap owns apply; there is no forwarding sync

Supersedes [0001](0001-daemon-reports-rules-forwarding-reconciles.md).

ADR 0001 split rule ownership in two: the daemon created and persisted rules,
and a separate forwarding sync diffed its desired set against the rules the
daemon reported. That sync lived in auto-discover, which derived its desired set
from Consul catalog state and Docker events and allocated host ports
dynamically when a service did not name one. Consumers were told the allocated
port, so a port could move to a different service while an nginx upstream still
pointed at the old one. That cross-serving was representable, and it happened.

auto-discover is gone. Its port allocation, bind-ip resolution, DNAT creation
and policy routes moved into `natmap::apply`.

## Decision

natmap owns application end to end. `natmap plan` and `natmap apply --from
<discovery.yaml>` read the node's declaration, diff it against the daemon's own
mapping state, and create, update and release mappings themselves.

- **Ports are declared, never allocated.** A declared port is the port used, on
  both sides of the DNAT. Ephemeral allocation stays only where it already was:
  `forwardlocal` without `bind_port`, and rproxy container targets. The
  declarative path never allocates.
- **The mapping set is the ownership ledger.** Each mapping records the service
  that owns its port. An apply refuses a port owned by another service, naming
  both, and releases a port only in the apply that removes its owner.
- **Only what an apply created is pruned.** A hand-added mapping has no owner
  and survives every prune; it is also never adopted silently.
- **Order is creates, then releases.** The exception is a port this apply hands
  to another service: that release runs first, because the daemon arbitrates
  the port.

There is no forwarding sync, so there is nothing for a second daemon to
reconcile. `GET /rules` remains a read-only view of the kernel for inspection
and lives nowhere in the diff path: the diff is declaration against the daemon's
persisted mappings, so an iptables rule installed by hand is not mistaken for
state natmap owns.

## Consequences

- One publishing path. A port cannot change owner without an apply that says so.
- Drift heals only when something runs `apply`; scheduling that is a separate
  decision, not a second reconciler.
- A node whose declaration is absent or empty converges to nothing, and prunes
  every mapping this node created. Deploying a node's declaration is therefore
  part of publishing on it.
- Hand-added mappings are outside the declaration's authority in both
  directions: never pruned, never adopted, and a conflict when they sit on a
  declared port.
