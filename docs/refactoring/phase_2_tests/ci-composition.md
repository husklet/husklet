# Phase 2 CI and Make composition

The current root targets route most behavior through `dd-tests`: `make test`, `test-ci`, `perf`, Docker
shell scenarios, Rust scenarios and coverage. Phase 2 replaces ownership without losing discoverability.

## Proposed root targets

| Root target | Composes | Network/device policy |
|---|---|---|
| `test` | default-member crate tests plus JIT quick matrix | offline; required engine artifacts |
| `test-jit` | `dd-jit` + `dd-jit-darwin` crate-owned matrix | mac engine lanes; no network |
| `test-daemon` | daemon unit/offline integration | private socket/state/image fixtures |
| `test-images-network` | registry integration against controlled service | explicit network job |
| `test-scenarios` | daemon quick cached real-image catalog | cached images; reports digest/arch |
| `test-scenarios-long` | daemon long/oracle catalog | opt-in network/heavy job |
| `test-render-headless` | GPU core + shims + display/compositor headless | no host GPU required |
| `test-render-mac` | wgpu/Metal + compositor live + GUI smoke | required mac GPU/GTK preflight |
| `test-package` | clean bundle/install/launch smoke | mac signing policy explicit |

The approved standalone benchmark target disappears. A temporary performance comparison may gate a hot-path
refactor, but it is not a permanent correctness suite or package owner.

## Migration of current targets

- `make test` and `test-ci` point to `dd-jit-darwin` after its matrix moves.
- Docker/compose/network/macOS-container/real-software shell targets invoke `dd-daemon`-owned testdata and
  runner paths.
- `scenarios*` invokes the daemon package's runner/test registry, not `dd-tests`.
- `coverage` moves with the JIT corpus because it scans engine implementations and executes JIT guests.
- `mac-crates` is corrected to acknowledge `dd-display` as a default member while still explicitly testing
  mac-only code and excluded compositor/wgpu crates.

## Job manifest

Each CI job uploads a small JSON manifest with repository commit, package, target triple, feature set,
toolchain, selected filters, required capabilities, artifact hashes and executed counts. The root summary
validates expected package/job names. This replaces fragile one-number claims such as “all 1636 tests”
while preserving traceability when tests move.

## No false-green policy

- mac-required CI fails when invoked on the wrong host; only developer convenience targets may print a
  nonzero-visible skip message.
- device/tool/compiler preflight happens once and aborts the required job before tests.
- tests do not return early as success when an executable, image, dylib or environment variable is absent.
- filters are validated against the registry and zero matches is an error.
- expected skips name an issue/owner and are included in the manifest; an increased skip count is red.
