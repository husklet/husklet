# hl-gl references/oracle

First-party, tracked parity oracles + conformance harnesses. Distinct from the
top-level gitignored `reference/` (external upstream clones).

- **gl_shim.c** — the retiring C clean-room GLES/EGL shim, kept as a *parity oracle*:
  the Rust shim's lowered IR is proven byte-identical to what it hand-emits.
- **conformance harnesses** — pixel/IR parity + capability-truth tests (the old
  `gui_matrix` GLES/EGL guests) that gate the driver against real apps (glmark2, ANGLE).
- **Mesa / Zink** — the strategic GL-over-Vulkan endgame reference: the long-term
  path is GL → Vulkan (via hl-vulkan) rather than a bespoke GL backend, so Zink's
  GL→SPIR-V lowering is the design oracle for that migration.
