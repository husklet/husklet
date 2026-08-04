# Vulkan blit: what is measured, what is decided, what to build next

Written 2026-08-01 from the first scored Vulkan CTS baselines. Numbers here were measured against bundle
`ebec5fdc65b6/2516dd295ad0`, guest ICD `4fff9173d7698959`, hashed inside the container and checked against
what the bundle stages. Full results and method: `../../../e2e/husklet/apps/vk-cts/COPY_AND_BLIT.md` and
`BASELINE.md` beside it.

**Current correction, 2026-08-03:** ordinary 2D block-compressed sources are now supported end-to-end.
The Vulkan recorder accepts them, and the host samples and decodes them into an uncompressed colour
destination. Block-compressed destinations remain refused because they cannot be colour attachments. The
2026-08-01 discussion below records the earlier un-advertising decision and must not be read as the current
compressed-source capability.

## READ THIS FIRST: the two sides disagree ON PURPOSE

**The CPU oracle serves 3D blits. The wgpu executor still refuses them.** Read cold, that looks like a bug
in one of them. It is neither — it is the intended intermediate state, and it is the thing that makes the
remaining work measurable.

A reference that cannot REPRESENT a case cannot validate the executor that will serve it: while both sides
decline, a differential agrees by mutual refusal and establishes nothing at all. So the oracle went first
deliberately, then the recorder. The executor is layer 3 and is UNSTARTED. The divergence closes when it
lands, and not before. It is stated at both refusal sites in the source
(`hl-gpu/src/cpu/executor/operation.rs`, `hl-gpu-wgpu/src/blit.rs`) so neither reads as an oversight.

| layer | where | state |
| --- | --- | --- |
| 1. IR + CPU oracle | `hl-gpu` | **landed** `e9de8e48f` |
| 2. Recorder + shim | `hl-vulkan` | **landed** `987aa7a79` |
| 3. Executor | `hl-gpu-wgpu/src/blit.rs` | **unstarted** — one draw per destination slice |

**Metal-side verification is cheap and proven, so there is no excuse for landing layer 3 unverified.**
Measured 2026-08-01: `mac cargo test -p hl-gpu-wgpu --test blit_mirror` builds incrementally in about 12
seconds and runs — `test mirrored_blit_reflects_each_axis_exactly ... ok`, `1 passed`. It genuinely
executed rather than being filtered out, which is the thing to check: `0 passed; 0 filtered out` and a
real pass look alike at a glance. `hl-gpu-wgpu`'s tests need Metal and CANNOT run in the Linux VM.

**A refused blit still takes its whole command buffer down, and that is PRE-EXISTING.** It is not
something these layers introduced — every one of the 54 `?` operators in `submit_cb_inner` has always
aborted the entire batch, and layer 2 only added one more way to reach it. Do not treat it as fallout from
this work or as a reason to unwind any of it. It is scoped, measured and handed over unstarted in
`../../gpu/hl-gpu-wgpu/SUBMIT_PROPAGATION.md`, which says to do it as its own commit BEFORE layer 3.

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

### LANDED 2026-08-01: layers 1 and 2 (`e9de8e48f`, `987aa7a79`). Layer 3 is unstarted.

The CPU oracle serves an unscaled depth-spanning blit, `Mirror` has its `z`, the recorder normalizes the
depth axis, and the `vkCmdBlitImage: 3D region` / `vkCmdBlitImage2: 3D region` refusals are gone. What
changed against the plan, measured rather than assumed:

* **There were TWO gates, not four.** `check_copy_subresource`'s z refusal and the `D3` refusal were
  real. The other two were not: a `D3` texture's slices ARE its planes, so `CopyBufferToTexture` already
  populated every slice and `CopyTextureToBufferRegion` with `sub.layer = z` already read any one back.
  `read_texture` returning `OutOfBounds` past the base plane is by design and was simply the wrong
  channel to measure through. `oracle_spec/blit3d.rs` proves that instrument first, with distinct
  content per slice, before any capability test leans on it.
* **The regression did not reappear.** `a_nearest_blit_converts_rather_than_reinterpreting_a_same_size_texel`
  passes. The form that avoids it: `Origin3d::z` names a PLANE and is spent exactly once, choosing the
  plane; the in-plane offset stays `(y * width + x)` relative to that plane's own start. A `depth: 1` D2
  texture resolves to `plane_at(0, 0)` = `0..w*h*bpt`, byte-identical to the old implicit base — so the
  predecessor's second untried lead ("does `plane_at(0,0)` return what the old code assumed?") is
  answered YES, and the first ("does `doz + dz` double-count?") is the trap that form avoids.
* **`Mirror::z` is wire bit 2 and is NOT a `WIRE_VERSION` bump.** It widens the accepted value set with
  the framing unchanged, and `Capabilities::negotiate` demands exact version equality, so no peer can
  mis-frame it. Reasoning is on `Mirror::to_u32`; reverse it there, not by inference.
* **`cargo check --workspace --all-targets` does NOT cover the Vulkan shim.**
  `src/surface/hl-vulkan/shim/vulkan` is its own workspace, absent from the root `members` list, and it
  held a live `Mirror` construction site the root check reported nothing about. The rule in `AGENTS.md`
  needs this second step: check the shim crates separately, or the enumeration argument is one radius
  short again.
* **A deliberate divergence now exists.** The oracle serves `D3` blits; `hl-gpu-wgpu` still answers
  "wgpu: 1D/3D blit source". That is the intended state for this slice and is recorded at both refusal
  sites. It is also the thing that makes layer 3 measurable.
* **z-scaled blits stay refused** as `Unsupported("software: depth-scaled blit")`. `VK_FILTER_LINEAR` is
  trilinear on a 3D blit; nearest-slice selection would be a plausible wrong answer that reads as a
  filtering difference. Do not "fix" it by picking a slice.

#### Layer 2 (`987aa7a79`), and what it cost to learn

`BlitRect` normalizes z alongside x and y, so a depth-spanning region is expressible at all; both `3D
region` refusals are deleted. Depth had been the raw `(a.z, b.z)` offset pair with a caller-side refusal
of anything but `(0, 1)` — a shape with no origin, no extent and no flip, which could only ever be
refused. A zero z span is now skipped as an empty region for the same reason a zero-width one is; before,
it reported "3D region", the same answer a legal depth-spanning blit got.

`cmd_blit_image` takes `Origin3d`/`Extent3d` instead of four `(u32, u32)` pairs. That is the
required-shape form — the compiler enumerated all fifteen call sites and none could keep the old meaning
by default. **It was landed on its own first**, with every hl-vulkan and shim test green, as the control
proving it moved no behaviour; only then were the three new refusals watched failing against real
plumbing. Recommended for layer 3 too: separate the plumbing from the rule, so the rule has something
honest to fail against.

Three rules, and the ORDER is load-bearing:

1. a non-3D image has no depth axis to span, refused by NAME — this must precede the bounds check. A 2D
   image's depth is one at every level, so a wider span would otherwise return `OutOfBounds`, which reads
   as "your region is too big" about a region that is the right size on an image with no third axis.
2. source and destination spans must be EQUAL, refused at record time where the caller can attribute it.
3. the span must lie inside the image's depth AT THE NAMED MIP, both sides. `ImageRec::depth_at` mirrors
   `extent_at` rather than returning `depth`, because Vulkan halves a 3D image's depth per level.

**The strongest argument in this repo for reverting rules INDIVIDUALLY rather than as a group.** Seven
rules were reverted one at a time and six were caught by their own test. The seventh — `Mirror::net`
dropping its z axis — **survived**, guarded by nothing anywhere in the tree. The prediction that the
recorder test would catch it was made from the code's shape and was WRONG: the recorder takes an
already-combined `mirror` and never calls `net`, which is the shim's job. The two live one call apart. A
z case now fails in the shim's own `a_mirrored_blit_region_keeps_its_flip_and_an_empty_one_is_skipped`.
A matrix written by reading the code rather than by running each reversion would have claimed that
coverage and been believed. Two other rules survived their first test for the same family of reason and
are documented in `oracle_spec/blit3d.rs` and `lowering/blit3d.rs`.

Not started: the executor (layer 3).

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
  it expected `1.0` — the destination looked unwritten.

  **My suspicion about it was CHASED AND REFUTED — do not spend time on it.** I guessed that `plane_at`
  sizing planes with `Texture::texel_bytes` while `blit_texture` indexes with `software_texel_bytes()`
  would misplace every offset past plane 0. Checked across every declared format: **they agree on every
  format the software backend can size.** Both delegate to `bytes_per_texel()`; `texel_bytes` differs only
  by mapping `Depth32Float`→4 and `Depth24PlusStencil8`→8, where `software_texel_bytes` refuses outright,
  so the software path never indexes with a stride the other sized. Pinned by
  `cpu/model/texture.rs::texel_size_agreement`, which fails if either function moves alone.

  So **the regression is something else and the next person should start fresh rather than anchored on
  this lead.** Untried directions, in the order I would take them: whether `dst_plane_starts` was keyed on
  `doz + dz` while the write offset already included the destination origin (a double-count that lands
  outside the plane for any non-zero origin), and whether `plane_at(0, 0)` on a `depth: 1` D2 texture
  returns the range the old code assumed implicitly. Both are cheap to check with a single-plane case,
  which is where the failure actually appeared.

  Note the shape of the invariant that survived: "safe because something else refuses first" is not the
  same as "correct", and it is the kind that rots silently — the day a depth format gets a software path,
  plane 0 will look right and every later plane will be off. That is why it is now a test rather than a
  comment.

### Layers

1. ~~**IR**~~ — **DONE** (`e9de8e48f`). `Extent3d::depth` and `Origin3d::z` already existed; only
   `Mirror::z` was missing, added as wire bit 2. NOT a `WIRE_VERSION` bump: it widens the accepted value
   set with the framing unchanged, and `negotiate` demands exact version equality, so no peer can
   mis-frame it and a bump would have invalidated other agents' in-flight bundles for nothing. The
   reasoning lives on `Mirror::to_u32` so it can be reversed on its merits.
2. ~~**Recorder**~~ — **DONE** (`987aa7a79`). See above.
3. **Executor** — **UNSTARTED**, and the only thing between here and the 352 cases.
   `hl-gpu-wgpu/src/blit.rs`. One draw per destination slice for the unscaled case; the refusal to delete
   is the `TextureDim::D3` half of the `D1 | D3` match, which now carries a comment explaining why it is
   still there. The recorder only ever emits `src.depth == dst.depth` (it refuses an unequal span by
   name), so the executor does not have to handle z-scaling — that would need a `texture_3d<f32>` binding
   and Z interpolation, and stays refused on both sides. Verify on the host; see the banner at the top of
   this file for the exact command and its measured cost. Do
   `../../gpu/hl-gpu-wgpu/SUBMIT_PROPAGATION.md` first, as its own commit.

**One enumeration warning that cost time on layer 1.** `cargo check --workspace --all-targets` does NOT
cover the shim crates — five of them declare their own `[workspace]`, sit outside the root `members` list,
and every one depends on `hl-gpu`. A required field added to `Mirror` produced a clean workspace check
while `shim/vulkan/src/transfer.rs` had an unbuildable literal in PRODUCTION code. This is now written up
in `AGENTS.md`; check the shims separately or the enumeration argument is one radius short.

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
