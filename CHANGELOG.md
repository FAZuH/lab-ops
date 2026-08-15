## [Unreleased]

### Added

- Added `GET /rules` endpoint to natmap daemon that returns live NAT rules.
- Added value `0` to `host_port`, which asks the natmap daemon for a free host port.

### Changed

- Changed host-port allocation so the natmap daemon assigns the ports.

### Fixed

- Fixed full sync failure removing services that were already registered.

## 0.1.28 (2026-07-12)


### New Features

* Support UDP port reservation in PortAllocator ([2fefbc0](https://github.com/FAZuH/lab-ops/commit/2fefbc07f9cb3927e4f2bd8a4f87ad2806f689a8))


### Bug Fixes

* Fix DNAT sync short circuiting on single failure ([1e4476c](https://github.com/FAZuH/lab-ops/commit/1e4476c2e5065fcd12c930bb8adc5d5fa5a62964))

## 0.1.27 (2026-07-09)


### ⚠ BREAKING CHANGES

* Remove deprecated nginx-sync subcommand

### New Features

* Store all domains in Consul Meta.domains ([6fe8054](https://github.com/FAZuH/lab-ops/commit/6fe805435f67bfa46e2bde58d1ca177e471d532d))


### Code Refactoring

* Remove deprecated nginx-sync subcommand ([e398049](https://github.com/FAZuH/lab-ops/commit/e398049b3071e0ea17ca895268dadc1d61f463f2))

## 0.1.26 (2026-07-05)


### Bug Fixes

* Fix cross-host traffic collision when multiple nodes expose the same port via natmap (e.g. port 9000 on sg-1). DNAT rules now filter by destination host IP. ([08d371d](https://github.com/FAZuH/lab-ops/commit/08d371d...)) ([f8d5af8](https://github.com/FAZuH/lab-ops/commit/f8d5af866d67efcf4d536dcc8ee3789d0c69c15e))

## 0.1.25 (2026-07-01)


### Bug Fixes

* Fix port reservation failures right after container restart ([2439b66](https://github.com/FAZuH/lab-ops/commit/2439b66edb1122ecf002adbea9b8a950aba009e9))

## 0.1.24 (2026-06-23)


### Bug Fixes

* Fix cannot access Docker containers via natmap localhost port mappings. ([f1cb5f9](https://github.com/FAZuH/lab-ops/commit/f1cb5f963948c89ffcb88ff44e544a252d05948f))

