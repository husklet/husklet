# Phase 2 helper dependency and runner design

## Cargo graph invariant

`dd-tests` is a leaf helper from the product graph's perspective:

```text
dd-tests (std + narrowly justified generic test utilities only)
   ▲          ▲          ▲          ▲
   │ dev-dep  │ dev-dep  │ dev-dep  │ dev-dep
dd-jit     dd-jit-     dd-daemon   GPU/shim/compositor crates
           darwin
```

It must not depend on `dd-jit`, `dd-jit-darwin`, `dd-daemon`, `dd-images`, GPU/display/shim crates,
CLI/GUI, or their concrete types. Otherwise moving ownership either creates a cycle or rebuilds unrelated
products for every helper test.

## Helper APIs that remain

| Module | Product-neutral responsibility |
|---|---|
| `command` | spawn with timeout, capture stdout/stderr/exit/signal, kill process group safely |
| `fixture` | temporary directory/root layout, atomic fixture writes, cleanup diagnostics |
| `guest` | invoke a supplied compiler command, cache by source+flags+compiler identity, record artifact manifest |
| `lane` | generic named lane metadata, required/optional state and selector validation |
| `result` | `Pass/Fail/Skip/Xfail/Xpass`, executed counts and structured failure evidence |
| `oracle` | normalize and compare two supplied command outcomes |
| `report` | deterministic human/JSON rendering without product names |

No module knows an engine filename, Docker socket, image store, Wayland display, GPU backend, architecture
binary path, workspace state root, or husklet environment variable.

## Owner adapters

- `dd-jit-darwin/tests/support` maps `Guest`/engine artifacts to generic lanes, provisions architecture
  toolchains and owns the C guest cache.
- `dd-daemon/tests/support` boots the daemon, creates private socket/state/image roots and maps scenario
  steps to Docker API calls.
- shim crates own shared-library build/dlopen helpers because sonames, target triples and ABI loading are
  product contracts.
- compositor tests own Wayland socket/client setup; wgpu owns Metal device availability and skip policy.

Adapters may share tiny helper functions through their owning crate's test support. Do not grow `dd-tests`
back into a facade that imports every product.

## Runner boundaries

Each owner may expose a developer runner, but `cargo test -p <owner>` remains the CI authority. A runner
must call the same registry/assertion code as integration tests rather than maintaining a second catalog.

| Owner | Optional runner purpose |
|---|---|
| `dd-jit-darwin` | filtering engine × guest matrix while retaining one Rust test registry |
| `dd-daemon` | selecting quick/long/oracle scenario cells |
| compositor/shims | compiling/running external C ABI clients when Cargo cannot express deployment setup |

Unknown filters and required lanes with zero executed cases exit nonzero. Platform unavailability is
decided before execution and emitted as structured skip evidence, never an early `return` from the test.

## Fixture ownership and deduplication

A fixture has one source owner. Cross-product consumers depend on its public test artifact through a
documented build step or copy a minimal independent black-box client when independence is the point.

- GPU IR byte corpora belong to `dd-gpu` and can be generated for shim tests.
- Khronos/CUDA ABI clients belong to their shim, not `dd-gpu-wgpu`; the host backend invokes the produced
  library/stream as an external input.
- Wayland protocol clients belong to `dd-compositor`; legacy parity can run the same built client against
  `dd-display` without duplicating source.
- Docker scenario definitions belong to `dd-daemon`; real Docker is an oracle backend, not another owner.

Every checked-in executable fixture needs source path, target triple, compiler/version, flags and SHA-256.
If reproducible locally, CI compares the rebuild; if the compiler is unavailable, CI validates the
manifest and the runtime behavior instead of pretending the binary is generated automatically.

## Dependency acceptance gate

After the split, `cargo metadata --no-deps` must show no workspace dependency from `dd-tests`. Each product
may list it only under dev-dependencies. `cargo tree -p dd-tests` must remain product-neutral, and removing
`dd-tests` from a production/package build must not change shipped artifacts.
