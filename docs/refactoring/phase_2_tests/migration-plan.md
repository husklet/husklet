# Phase 2 migration and gates

## Step 0 — freeze semantics

Record current case IDs, selected engine lanes, fixtures, expected outputs, skip reasons and CI commands.
Capture counts per owner, not one repository-wide magic total. Remove the source-substring tests identified
in [`deep-test-value-wave-ab`](../phase_1_audit/research/deep-test-value-wave-ab-2026-07.md) only when their behavioral
replacements exist.

## Step 1 — make `dd-tests` a helper API

First remove the helper crate's product dependency: expose stable product-neutral modules for supplied
compiler commands, temporary roots, command timeouts, native differential execution, lane selection and
structured results. Engine discovery belongs to a `dd-jit-darwin` adapter, not the helper. Remove product
registries from its public API. Add tests that deliberately provoke compiler absence, missing supplied
command, timeout, crash, bad expected output and zero selected lanes.

## Step 2 — move the JIT corpus

Move case definitions, C guests, LTP, pcache/forkserver/overlay/non-PIE tests and the aggregate runner to
`dd-jit-darwin`. Keep `dd-jit` tests limited to its host-neutral Rust API. Preserve stable case IDs so
failure history remains searchable. Both Linux guest architectures must report executed/pass/fail/skip;
Darwin guest tests remain a distinct lane rather than being inferred from a Linux run.

Gate: `cargo test -p dd-jit-darwin --all-targets` plus the explicit engine matrix for every required
artifact. Missing engines or zero executed cells fail before test execution.

## Step 3 — move daemon and image journeys

Move the Rust scenario runner, scenario catalog and daemon-oriented shell flows to `dd-daemon`. Refactor
daemon boot/socket/state/image-root setup into `dd-daemon` test support. PostgreSQL, Redis, language,
toolchain, compose, network, volume and real-image cases are daemon acceptance tests. Registry/archive
mechanics that do not launch a container belong to `dd-images` instead.

Gate: daemon unit/integration tests run offline fixtures first; cached real-image quick tests are explicit;
network-pulling long tests are a separate opt-in/CI job. Oracle-vs-husklet mode must execute the same case
definition and report image digest and architecture.

## Step 4 — split rendering by API boundary

Move IR/wire/software tests to `dd-gpu`, transport tests to `dd-shim-common`, EGL/GLES guests to
`dd-shim-gl`, Vulkan guests to `dd-shim-vk`, CUDA guests to their shim, Wayland clients to
`dd-compositor`, and real Metal lowering to `dd-gpu-wgpu`. A cross-boundary journey is owned by its final
observable product: e.g. Vulkan-to-Metal execution is a `dd-gpu-wgpu` test that uses a Vulkan fixture.

Gates are headless Rust tests for IR/shims, C ABI clients for exported libraries, compositor protocol and
pixel tests, then the macOS Metal/IOSurface gate. Manifest/export census is build evidence; it does not
replace calling the API.

## Step 5 — split root CI orchestration

Root CI/Make targets call crate-owned gates in dependency order and preserve parallelism:

1. pure/headless crates;
2. JIT engine matrix;
3. daemon offline integration;
4. guest shim ABI and render headless tests;
5. macOS compositor/wgpu/GUI tests;
6. opt-in real-image/network acceptance.

Each job emits an executed-count manifest. A required job with zero tests, an unavailable required engine,
an unknown filter, or an unexpected skip is red. Expected platform skips are named and counted.

## Step 6 — delete aggregate ownership

Only after destination parity is demonstrated, remove the old registrations, duplicate fixtures,
`scenarios` binary, and product test modules from `dd-tests`. Run `rg` for old fixture paths and ensure
Make/CI/docs no longer invoke them. The helper crate remains; the monolithic test product does not.

## Patch discipline

Move one ownership group per commit. First make the destination pass, then remove the source in the same
commit. Do not rename to husklet during this phase. Avoid source-text assertions; use Rust assertions and C
ABI fixtures against observable results. Performance checks may prove a move did not regress setup/runtime,
but no permanent benchmark suite is introduced.
