# Phase 2 — split test ownership

Status: detailed implementation plan; no files have been moved.

## Target architecture

Every product crate owns the tests for its public and internal behavior. `dd-tests` becomes a small,
generic support package: process execution, engine-lane selection, guest compilation/provisioning,
temporary roots, result normalization, and reusable assertions. It must not own Docker-daemon scenarios,
image workflows, GPU semantics, compositor behavior, or a catalog of JIT instructions/syscalls.

```
owning crate/tests + owning crate/testdata
        │
        └── uses dd-tests helpers when it needs guest compilation or engine matrix execution

dd-tests
  src/harness/       reusable runner primitives
  src/fixtures/      generic temp/rootfs/guest-build helpers
  src/oracle/        native-vs-engine result comparison
  tests/             tests of the helper package itself only
```

`dd-tests` may depend on `dd-jit` to offer an engine harness. Product crates should use it as a
`dev-dependency`; `dd-tests` must not depend on `dd-daemon`, GPU shims, display, or GUI products.

The current-tree evidence is in [`research/current-test-inventory.md`](research/current-test-inventory.md).
The file-level destination is in [`ownership-matrix.md`](ownership-matrix.md). The migration order and
required gates are in [`migration-plan.md`](migration-plan.md).

## Ownership rules

1. The crate that can break the behavior owns the test.
2. Cross-crate integration belongs to the highest-level product boundary under test. Docker API and
   real-image behavior belong to `dd-daemon`; Metal execution belongs to `dd-gpu-wgpu`; Wayland protocol
   and composition belong to `dd-compositor` or the still-live legacy `dd-display` path.
3. Guest instruction, syscall, loader, process, pcache, forkserver, overlay, and Linux-ABI probes belong
   to `dd-jit-darwin`, because that engine implements them. Generic runner machinery remains reusable.
4. C sources that call EGL/GLES/Vulkan/CUDA stay beside the corresponding shim as ABI fixtures. A C
   fixture is not a reason for `dd-tests` to own the assertion.
5. Unit tests may remain next to source. Larger behavioral tests use `<crate>/tests`; input programs and
   golden data use `<crate>/testdata`, not a global guest landfill.
6. A skipped platform lane is reported as skipped with a reason and required-lane count. A selected suite
   that executes zero tests fails.

## Definition of done

- `dd-tests/src/scenarios`, its `scenarios` binary, and daemon shell scenarios have moved to `dd-daemon`.
- JIT case registration and guest corpus have moved to `dd-jit-darwin`; only generic harness APIs remain.
- rendering IR/backend tests are owned by GPU/transport crates.
- each crate has a documented `cargo test -p <crate>` contract; macOS-only crates have an explicit mac gate.
- root CI composes crate-owned gates and reports executed counts; it does not hide ownership in one matrix.
- no correctness test reads `.rs`, `.c`, or generated source merely to assert text/symbol presence.
