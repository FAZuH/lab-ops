# 31 — natmap daemon allocates on the mapping flow

**What to build:** the natmap daemon's mapping endpoint accepts a mapping request without an explicit host port and allocates one from its `PortAllocator`, returning the chosen port in the response. When the requested host port is already taken, it returns 409 exactly as today. The typed client can send the no-port form and read back the allocated port.

**Blocked by:** None — can start immediately.

**Status:** ready-for-agent

- [ ] Mapping request with no host port → daemon allocates a free port via `PortAllocator` and returns it in the response
- [ ] Mapping request with a taken host port → 409, unchanged
- [ ] Typed client sends a mapping request without a host port and reads back the allocated port
- [ ] Allocate→install→rollback still holds (an allocated port is released if the install fails)
- [ ] Unit-tested over the in-process router with a fake iptables adapter; natmap lib suite green
