# Vulkan blit: what is measured, what is decided, what to build next

Written 2026-08-01 from the first scored Vulkan CTS baselines. Numbers here were measured against bundle
`ebec5fdc65b6/2516dd295ad0`, guest ICD `4fff9173d7698959`, hashed inside the container and checked against
what the bundle stages. Full results and method: `../../../e2e/husklet/apps/vk-cts/COPY_AND_BLIT.md` and
`BASELINE.md` beside it.

## The state in one paragraph

`dEQP-VK.api.copy_and_blit.core.*` is scored exhaustively: 134,125 cases, 0 unrun. 1,172 of its failures
were a capability **advertised at query time and refused at record time**, which is worse than never
advertising — an application that checks `VkFormatProperties` correctly still fails. Those 1,172 are three
causes, not one. One is fixed (`d471e287a`). Two remain, and the ordering below is not the order of case
counts.

| cause | cases | status |
| --- | --- | --- |
| compressed / depth-stencil: no packed colour texel | 636 | **624 fixed** by un-advertising; 12 depth/stencil untouched |
| `vkCmdBlitImage: 3D region` | 352 | open — **fund first** |
| blit of an integer format | 100 | open — second |
| mipmap / depth-stencil / `simple_tests` variants | 84 | follow the above |

## Decisions a newcomer will be tempted to reverse

**3D blits cannot be declined, so they are not rankable by case count.** `VkFormatFeatureFlags` is per
FORMAT, not per image type. There is no bit to withdraw and no query through which an application could
discover that this driver refuses a depth-spanning blit. Every other gap on this list can be made honest by
advertising less; this one cannot. It is a core Vulkan 1.0 operation and the only *unannounceable* hole in
the driver, which is why it is funded ahead of larger-looking items.

**BC could be un-advertised for free; integer cannot.** These look like the same fix and are not. Both were
advertised and refused. But `BLIT_SRC` becomes mandatory for a compressed format only when its required set
includes `SAMPLED_IMAGE_FILTER_LINEAR`, which follows from `textureCompressionBC` — which this driver does
not advertise, so it was optional and dropping it was free. Integer formats are different:
`a8b8g8r8_uint_pack32`, `r32_uint`, `r16g16_sint` and others are already in the CTS's *required-and-missing*
set, so dropping their blit bits would convert 100 `copy_and_blit` failures into new `api.info` failures.
The total would not improve and the driver would be less truthful. **Do not "fix" integer blits by
un-advertising them.**

**The format table is wrong in BOTH directions at once.** 1,172 cases advertised-and-refused; 30 formats
required-and-missing. A change that only widens, or only narrows, makes the other half worse. Neither suite
alone can see the damage the other measures — `api.info` catches required-and-missing, the `copy_and_blit`
groups catch claimed-and-refused. The warning is on `features()` in `src/model/capability.rs`; read it
before editing the table.

**Never quote the two scoring bases interchangeably.** On this slice they are 27.7% (executed) and 97.8%
(enumerated), because 97% of it is `NotSupported`. Only 4,082 of 134,125 cases execute at all. Either
number alone is indefensible: one describes a driver that fails most of what it runs, the other a driver
that passes almost everything, and both are true of the same run.

**Projected, not measured — re-run before citing.** After `d471e287a` the projections are 32.7% executed
and 98.3% enumerated. They are arithmetic over a status flip verified on two 624-case groups plus the
`api.info` control holding at exactly 184; the full 134,125-case re-run has not been done. The numerator is
unchanged — **1,131 cases passed before and 1,131 after**. Nobody should read the executed rate moving five
points as new capability: it is the same driver, having stopped claiming a blit it never performed. Replace
these figures with measured ones; do not let a projection become a quoted number by repetition.

## Slice 1 — 3D depth-unscaled blit (funded)

**Definition.** Depth-spanning blit regions where `src_extent.depth == dst_extent.depth`. Each destination
slice reads exactly one source slice, so there is no Z resampling, the existing 2D draw is reused per
slice, and **no new shader is required**. Leaves Z-scaled blits refused.

**Build the CPU oracle FIRST.** An executor validated against an oracle that cannot represent the case is
not validated at all — while both sides decline, a differential agrees by mutual refusal and establishes
nothing.

### An attempt was made and reverted. Read this before repeating it.

The oracle change was written, compiled, and **reverted** because it regressed an existing test with
context exhausted for debugging. Nothing is committed; the tree is clean and `oracle_spec` is 28/28. What
the attempt established is worth more than the diff:

* **The storage was never the limit.** `cpu/service/copy.rs:121` says the oracle "materializes only 2D,
  single-layer, level-0 color textures". **That comment is false and it cost an estimate outright.**
  `cpu/model/texture.rs` stores pixels LAYER-MAJOR and documents its planes as "depth slices for a 3D
  one", with `plane_at(mip, layer)` returning each byte range. Fix this comment in the same change; it is
  the third load-bearing false comment this fleet hit in one day.
* **There are FOUR gates, not the two originally scoped.** Beyond `check_copy_subresource` (rejects
  `origin.z != 0 || depth > 1`) and the `TextureDim::D3` refusal in `cpu/executor/operation.rs`, two more
  surfaced only by running: the copy path **cannot populate** a depth slice, and `read_texture` **cannot
  read one back** — it returns `OutOfBounds` for the second plane. So a per-slice content assertion needs
  all four lifted. A test asserting acceptance and in-bounds traversal is possible without them; a test
  asserting *slice z receives slice z* is not.
* **`Mirror` has no `z`.** It carries `x` and `y` only, so a flipped depth axis is inexpressible today. Add
  `Mirror::z` in the same IR change that adds the depth extent — otherwise the oracle silently ignores a
  flip the caller asked for, which is exactly the failure it exists to catch.
* **The unresolved regression**, for whoever picks this up: with per-plane addressing wired through
  `plane_at`, `blit::a_nearest_blit_converts_rather_than_reinterpreting_a_same_size_texel` read `0.0` where
  it expected `1.0` — the destination looked unwritten. Suspicion, unverified: `plane_at` sizes planes with
  `Texture::texel_bytes` while `blit_texture` indexes with `format.software_texel_bytes()`, and the two
  disagreeing would misplace every offset past plane 0 while leaving plane 0 apparently fine. Check that
  before rewriting anything.

### Layers, once the oracle is in

1. **IR** — `hl-gpu/src/protocol/model/descriptor.rs`. `BlitTexture` regions carry a 2D rect plus a layer
   range; they need a depth extent and origin, plus `Mirror::z`. This is a wire change: take the form where
   the compiler enumerates every construction site. That discipline caught an agent today who had checked
   only one crate, so run `cargo check --workspace --all-targets`.
2. **Recorder** — `hl-vulkan/src/service/record/image.rs` and `shim/vulkan/src/transfer/copy.rs:236`. Stop
   collapsing the depth axis; delete the pre-recorder refusal that latches
   `Unsupported("vkCmdBlitImage: 3D region")`.
3. **Executor** — `hl-gpu-wgpu/src/blit.rs`. One draw per destination slice for the unscaled case. Z-scaled
   blits need a `texture_3d<f32>` binding and Z interpolation; that is slice 2, not slice 1.

## Slice 2 — integer blits

**Measure the scaled/unscaled split first, and do not derive it from case names.** A name-based split was
attempted for the texel-buffer gap and misfiled 56 of 88 cases, because a case name names the resource
under test rather than what the test compiles. The `latch()` diagnostic added in `d471e287a` now logs every
record-time refusal with its reason at `error` level under `tag::VULKAN`, so run the 100 cases with
`HL_VK_LOG=all` and count the real thing.

**The cost estimate was corrected once — do not revert to the cheap one.** This was first called "a
filtered-copy path without the filter". It is not. **wgpu has no native image blit**: `blit.rs` implements
one by SAMPLING the source through a full-viewport draw, with a hardcoded
`@group(0) @binding(0) var src_tex: texture_2d<f32>` (blit.rs:42) and a `TextureSampleType::Float` layout
(blit.rs:124). An integer texture binds to neither and cannot go through a filtering sampler at all.

Serving it needs: two more WGSL variants (`texture_2d<i32>`, `texture_2d<u32>`) with `vec4<i32>`/`vec4<u32>`
fragment outputs, because wgpu requires the fragment output type to match the target's sample type — the
same rule behind the six signedness refusals in `BASELINE.md`; sampler-less bind layouts using
`textureLoad`; the pipeline cache key extended from `(format, can_filter)` to include sample type
(blit.rs:207); and the resampling arithmetic moved into the shader, matching
`hl-gpu/src/cpu/service/copy.rs::blit_texture` exactly. All ours, nothing vendored. A day in the executor.

**A first slice worth landing alone:** the 1:1 unscaled case, where an integer blit is a plain
`copy_texture_to_texture` needing no shader. Small, cannot regress the float path, testable against the
oracle.

## Open items

* **The 12 depth/stencil cases** inside the 636. `FormatClass::DepthStencil` still advertises `BLIT_SRC`
  and `BLIT_DST` while the recorder refuses a single-aspect blit of a combined depth/stencil image. Whether
  those bits are honest was **not tested** by the BC change and is unresolved. Same question, same table,
  different class — and the mandatory-feature check must be redone for it, because the BC answer does not
  transfer.
* **Re-score `copy_and_blit.core`** against a post-`d471e287a` bundle and replace the projections above.
* **`image_clearing`** (45,636 cases) and the non-`core` copy variants (67,221) remain entirely unmeasured.
* **`dEQP-VK.api.object_management.*`** (457) has no verdicts at all — it hangs the guest exec channel in a
  way a per-case timeout does not bound. A location, not a diagnosis.
