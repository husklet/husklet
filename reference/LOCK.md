# Reference provenance lock

This file pins the exact upstream revisions of the external projects that dd cites
as authorities. It exists to satisfy `docs/codex-rendering.md` §9.1 ("Reference
provenance must be pinned"): `dd-shim-vk` comments cite MoltenVK classes and source
files line-by-line, so those references must be pinned or the citations cannot be
reproduced and upstream drift stays invisible.

## The rule (§9.1)

- These are **READ-ONLY** references. **Never** modify a reference tree to make dd
  tests pass. dd adaptations belong in dd crates or an explicitly documented
  `third_party` patch series — not here.
- Reference updates are their **own reviewable commits**. The update procedure is:
  1. update the pin (bump the SHA in this file),
  2. regenerate manifests,
  3. run the semantic-diff checklist against the old revision,
  4. only then port selected behavior into dd crates.
- Never mix a large upstream refresh with behavioral dd changes (§9.6 step 6).

## Pinned revisions

All SHAs below are the checked-out `HEAD` of the corresponding local mirror under
`/Users/x/vk-refs/<repo>` at the time of vendoring (pinned 2026-07-12).

| Reference | Pinned commit | Upstream origin | License |
|---|---|---|---|
| MoltenVK | `5a4e526ee8f46ac0cebdd516b8b900562b80828e` (v1.4.2, dated 2026-07-11) | https://github.com/KhronosGroup/MoltenVK.git | Apache-2.0 |
| Vulkan-Loader | `7ab4d368d8499f3366728a6991eec910d9c34ae4` (dated 2026-07-09) | https://github.com/KhronosGroup/Vulkan-Loader.git | Apache-2.0 (with a few more-permissive per-file exceptions) |
| Vulkan-Headers | `8d6039a455a7ecc7d2a592ff97f62db4e59b70bf` (Vulkan-Docs 1.4.356, dated 2026-07-03) | https://github.com/KhronosGroup/Vulkan-Headers.git | Apache-2.0 OR MIT |
| SPIRV-Cross | `6c09849fe88c48eaed08413aa022aaa136a3a057` (dated 2026-07-06) | https://github.com/KhronosGroup/SPIRV-Cross.git | Apache-2.0 |
| ash | `a9a1fb17e98a0cde146caada86200d809306200d` (dated 2026-05-20) | https://github.com/ash-rs/ash.git | Apache-2.0 OR MIT |

## What is vendored here vs. pin-only

- **MoltenVK** — the cited source subset is vendored under `reference/moltenvk/`
  (see `reference/moltenvk/DD-README.md`). Only the subtrees that `dd-shim-vk`
  cites are copied (GPUObjects, Commands, API, the `Vulkan/vulkan.mm` ICD entry
  point, and the `MoltenVKShaderConverter` SPIRV→MSL converter), plus `LICENSE.md`
  and `README.md`. `Externals/`, `Demos/`, build artifacts and doc images are
  intentionally excluded to keep repo size sane.
- **Vulkan-Loader, Vulkan-Headers, SPIRV-Cross, ash** — pin-only in this file. Their
  full trees are large and are consumed as a build dependency / generated inputs, not
  cited line-by-line. If a future dd comment starts citing one of these by file+line,
  vendor the cited subset the same way MoltenVK was vendored and note it here.

## Verifying a pin

```sh
git -C /Users/x/vk-refs/MoltenVK      rev-parse HEAD   # -> 5a4e526e...
git -C /Users/x/vk-refs/Vulkan-Loader rev-parse HEAD   # -> 7ab4d368...
git -C /Users/x/vk-refs/Vulkan-Headers rev-parse HEAD  # -> 8d6039a4...
git -C /Users/x/vk-refs/SPIRV-Cross   rev-parse HEAD   # -> 6c09849f...
git -C /Users/x/vk-refs/ash           rev-parse HEAD   # -> a9a1fb17...
```
