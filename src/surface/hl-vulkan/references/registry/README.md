# references/registry — Khronos Vulkan registry + extractors (first-party, tracked)

Non-Rust support pulled from the current `hl-shim-vk/registry/`. These are the source of
truth for the shim's `#[no_mangle]` export set and the census/anti-drift test.

Moves here on implementation:

- `vk.xml` — the Khronos Vulkan API registry (command signatures, versions, extensions).
- `extract_vk_core_commands.py` — extractor: vk.xml → the core command list.
- `extract_vk_manifest.py` — extractor: vk.xml → full command manifest (signatures).
- `extract_vk_origins.py` — extractor: vk.xml → per-command core-version/extension origin.
- `vk_commands.manifest` — full command signature manifest (sidecar).
- `vk_command_origins.manifest` — per-command origin sidecar (drives truthful stub errors).
- `vk_core_commands.manifest` — the core command census sidecar (build.rs + census test).

Distinct from the top-level gitignored `reference/` (external upstream clones): this
`references/` is first-party, tracked, and diffed against the shim exports (D5).
