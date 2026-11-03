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
  both sides of the DNAT. Ephemeral allocation survives only in the retired
  forwarding path (`forwardlocal` without `bind_port`, rproxy container
  targets); the declarative path never allocates.
- **The mapping set is the ownership ledger.** Each mapping records the service
  that owns its port. An apply refuses a port owned by another service, naming
  both, and releases a port only in the apply that removes its owner.
- **Only what an apply created is pruned.** A hand-added mapping has no owner
  and survives every prune; it is also never adopted silently.
- **A declared mapping outlives containers.** It is stored under a `service:`
  key, never a container id, and a reload replays it from its stored addresses
  whatever Docker reports: no container id matches it and no inspect can
  retarget it. Only an apply removes it.
- **A locally delivered destination holds a claim, not a listener.** When the
  mapping forwards to an address on this node the kernel rule alone services
  it, so the daemon claims the host address in its allocator instead of binding
  it. The claim keeps the 409 conflict gate; the listener would compete with
  the rule, and with the service holding the port being forwarded to.
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
- A restart is not a re-publish: the declaration's mappings come back from state
  before anything asks about them, so `plan` after a restart reports converged.
- Drift heals on a timer that runs `apply`, not from a loop inside natmap:
  `natmap install --from` writes a systemd timer whose oneshot unit runs
  `natmap apply --from <file> --prune`. The schedule is systemd's, so there is no
  second reconciler, no cursor and no "last applied" file that could disagree
  with the daemon's mappings — and a converged tick writes nothing at all.
- **Forwarding is retired, deliberately.** With auto-discover gone, no component
  turns "service on node X" into "public port on node Y": the
  `forwardlocal`/`forwardremote` blocks and the hairpin rules they produced are
  gone from the declarations and will not come back. Public non-HTTP ingress is
  either a rendered nginx `stream:` block on the fronting node, or a mapping
  applied locally on the node that owns the address. A node publishes only the
  ports it owns, and nothing allocates one on its behalf.
- A node whose declaration is absent or empty converges to nothing, and prunes
  every mapping this node created. Deploying a node's declaration is therefore
  part of publishing on it.
- Hand-added mappings are outside the declaration's authority in both
  directions: never pruned, never adopted, and a conflict when they sit on a
  declared port.
