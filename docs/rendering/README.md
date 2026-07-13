# Rendering documentation

This directory contains only current rendering documentation and executable reproduction recipes.

## Authoritative documents

- [`../codex-rendering.md`](../codex-rendering.md) — current completeness audit, remaining behavior gaps,
  priorities, and required evidence. This is the primary rendering status document.
- [`CHROME-FIX-PLAN.md`](CHROME-FIX-PLAN.md) — active multi-process Chrome engine root cause and fix plan.
- [`SHIM_RUST_ARCHITECTURE.md`](SHIM_RUST_ARCHITECTURE.md) — maintained guest GL/Vulkan/CUDA shim architecture.
- [`SHIM_GL_COMPLETENESS.md`](SHIM_GL_COMPLETENESS.md) — generated GL/EGL symbol and capability census. It proves
  inventory only; runtime correctness requires behavioral tests.
- [`../GOLDEN_HARNESS.md`](../GOLDEN_HARNESS.md) — Metal golden-image harness and update procedure.

Historical debugging diaries, branch-specific handoffs, screenshots, and superseded readiness/checklist documents
were removed. Git history remains the archive.

## Reproduction recipes

- [`chromium-workspace-repro.sh`](chromium-workspace-repro.sh) — repair and launch the coherent Chromium workspace.
- [`gtk4-workspace-repro.sh`](gtk4-workspace-repro.sh) — build and launch a coherent glibc GTK4 workspace.
- [`vulkan-workspace-repro.sh`](vulkan-workspace-repro.sh) — build and launch the software-Vulkan/lavapipe workspace.

These scripts mutate local images/workspaces and may use Docker or the macOS bridge. Read their headers before use.

## Required validation

After rendering or shared-type changes:

```sh
cargo test -p dd-tests --test rendering_ir --test rendering_backends
make mac-crates
```

For pixel correctness on a Metal-capable Mac:

```sh
./run_golden.sh
```

Source-string searches are not rendering tests. New gates must execute a public ABI, Wayland/socket exchange,
backend state transition, timing path, or pixel comparison. Findings without such a harness remain backlog items in
`codex-rendering.md` rather than ignored tests.
