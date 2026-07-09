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

## 0.1.23 (2026-06-02)


### Bug Fixes

* Add LAN hairpin MASQUERADE for preserve_src_ip ([585273f](https://github.com/FAZuH/lab-ops/commit/585273fb341e533a8037d6287ea36a1661ca692b))
* Fix natmap networking error on ForwardRemote with ([c539f43](https://github.com/FAZuH/lab-ops/commit/c539f43920e3e9fd6d4f9c3edfc030be0f112b7a))

