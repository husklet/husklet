# hl-gl references/registry

First-party, tracked registry inputs that drive the shim's generated entry-point
surface. Distinct from the top-level gitignored `reference/` (external upstream clones).

Moves here from `hl-shim-gl/registry/` on cutover:

- `gl.xml`, `egl.xml` — Khronos registry XML (the source of truth for the symbol set).
- `extract_gl_core_commands.py` / `extract_from_khronos.py` — extractors that render
  the XML into flat command manifests.
- `gles2_egl.manifest`, `gl_core_commands.manifest` — generated `.manifest` sidecars:
  the exact egl*/gl* export set, diffed against the shim's `#[no_mangle]` exports by the
  crate's census/anti-drift test.

`build.rs` consumes the manifest to generate the C-ABI export surface + capability inventory.
