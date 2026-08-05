# Legacy API group ownership audit

The removed Rust modules under `tests/scenarios/groups/` were detached API
orchestration, not folder-owned YAML end-to-end definitions. Their public
contracts are owned and exercised by the packages below. Declarative category
loaders remain in place until their YAML migrations are independently audited.

| Removed module | Owning public-contract coverage |
|---|---|
| `copy.rs` | `hl-daemon/tests/api/container_copy.rs`, `hl-container/tests/filesystem_coherence.rs`, and typed stat/copy/export coverage in `hl-client/tests/daemon/observability.rs` |
| `execcmd.rs` | `hl-container/tests/process_contract.rs` and `hl-client/tests/execution.rs` |
| `imagescmd.rs` | `hl-client/tests/daemon/image.rs` and daemon image archive/prune tests |
| `netcontainer.rs` | live name-based traffic in `hl-daemon/tests/api/network_bridge.rs` and endpoint/IP/alias ownership in `hl-container/tests/networks.rs` |
| `network.rs` | container network tests, daemon network list/bridge/built-in tests, and typed client network tests |
| `runflags.rs` and `runflags_docker.rs` | `hl-container/tests/run_options.rs`, process contracts, and daemon/client create, network, publication, resource, and removal tests |
| `volume.rs` and `volume/` | `hl-container/tests/volumes.rs`, daemon volume/system-disk tests, and typed client volume tests |

The removed Redis, netcat, ping, and shell command choices were vehicles for
the package contracts above, not separate reusable API behavior. Repository
application and multi-package scenarios belong in direct child folders with a
discoverable `test.yaml`, local inputs, and local goldens.

`observe.rs` is closed. Inspect, list, logs, archive, publication, prune, inactive
observability, and option-validation contracts were already package-owned. The
remaining live successful `top` and one-shot `stats` behavior now runs through
the typed client and daemon against a real pinned Alpine process in
`hl-client/tests/daemon/observability.rs`. The detached `groups/` registry and
runner have therefore been deleted.
