# Compositor shared-mechanics audit (wave V, 2026-07)

This audit compares the hand-written `dd-display` Wayland server with `dd-compositor`'s Smithay path.
It identifies mechanics that can be centralized before compositor cutover without changing protocol
policy. “Shared” below means one implementation with byte-for-byte output for the same normalized input;
similar names or formulas are not enough.

## Immediate, behavior-preserving cuts

| Priority | Shared contract | Current duplication | Safe extraction and proof | Runtime consequence |
|---|---|---|---|---|
| P0 | dd dmabuf modifier layout | `DD_DMABUF_MOD_MAGIC`, render bit and high/low decoding occur independently in both compositors | Put the two constants plus `encode_dd_modifier`/`decode_dd_modifier(u64) -> Option<DdBufferIdentity>` in a dependency-neutral `dd-display::dmabuf_contract` module. Use it in legacy parsing, Smithay validation and feedback construction. Exhaustively test tag-bit boundaries and all `u32` IOSurface ids. | Tiny pure functions should be `#[inline]`; LLVM eliminates the call. One decode per buffer creation, never per pixel/frame. |
| P0 | DRM formats and synthetic render-node identity | ARGB/XRGB fourccs and render device `226:128` are restated while feedback tables are separately serialized | Share typed format/capability rows and `DD_RENDER_DEVICE_ID`. Keep protocol-specific table emission local. Assert legacy and Smithay traces advertise the identical ordered rows and native-endian `u64` device bytes. | Startup/bind only; no measurable frame cost. Static slices avoid allocation. |
| P0 | presentation clock/refresh conversion | Linux monotonic clock id `1`, nominal 60,000 mHz and refresh conversion are duplicated; timing evidence types already live in `dd-display::present` | Move clock id, mHz-to-ns checked conversion and nominal refresh into `present`. Both protocol adapters translate the same `PresentTiming`; retain callback ownership locally. Test `0`, 60 Hz, variable refresh and overflow. | `const fn` conversion folds at compile time. Evidence translation is once per delivered frame and already crosses crate boundaries. |
| P1 | eight Wayland buffer pixel transforms | Both paths perform the same inverse pixel mapping, but use `i32` versus Smithay `Transform`, owned versus borrowed buffers, and different malformed-length behavior | Define a small local `BufferTransform` enum and one checked `transform_bgra(&[u8], w, h, transform) -> Result<Cow<[u8]>, _>`. Protocol adapters validate/convert their wire enum. Golden-test every transform on asymmetric 2×3 pixels and malformed dimensions/lengths; compare old outputs before replacement. | This is a pixel hot path. Put it in the existing lower-level display crate, mark only the coordinate mapper `#[inline(always)]`, preserve the normal-transform borrowed fast path, and benchmark generated assembly only to guard regression—not as correctness evidence. |
| P1 | premultiplied BGRA source-over kernel | Both paths implement identical per-channel integer source-over and XRGB opacity, but traversal/sampling differs | Extract only `blend_premul_pixel(dst: &mut [u8;4], src: &[u8;4], opaque: bool)`. Keep clipping, scale, viewport sampling and tree traversal local. Exhaustively compare all alpha values and representative channels against the existing formula, including forced destination alpha. | Four-channel inner loop: require `#[inline(always)]`, no slice bounds checks in generated loop, and no function-pointer abstraction. The extraction can reduce code without adding a call. |
| P1 | Cocoa event normalization | Two Cocoa loops duplicate flip/scale, mouse button codes, scroll sign and precise-wheel classification | Produce a backend-neutral `HostInputEvent` from `NSEvent` once, then dispatch it to either compositor. Test normalization with plain Rust values; keep Smithay focus/constraints and legacy wire emission separate. | One enum match per host event is negligible relative to AppKit dispatch. Avoid allocation/boxing; use a small `Copy` enum. |

These five extractions remove approximately 120–180 lines immediately and, more importantly, make the
dmabuf identity and transform math single contracts. Exact deletion count depends on whether compatibility
wrappers remain during migration. Do not create a new crate: `dd-compositor` already depends on
`dd-display`, whose `present` API already carries shared timing evidence.

## Similar-looking paths that must remain separate

| Area | Why it is behavior-divergent |
|---|---|
| Full blending/composition | Legacy `blend_subsurface` is 1:1 backing-pixel clipping. Smithay `blend` applies logical/device scale, viewport destination and UV crop, and also composes popup/subsurface trees. Sharing the traversal would either lose Smithay semantics or silently change legacy output. Share only the pixel kernel. |
| Transform damage | Smithay transforms damage rectangles after transforming the cache; legacy does not have an equivalent damage-upload contract. Centralizing it would invent legacy behavior, not remove duplication. |
| dmabuf validation/import | Smithay validates flags, plane count/index, offsets, stride, backing size and IOSurface metadata. Legacy closes a placeholder fd and records only modifier/stride before later resolution. Centralize capabilities and identity decoding, then separately raise legacy validation to Smithay parity. Do not “share” by weakening Smithay. |
| dmabuf feedback encoding | Legacy manually writes protocol arrays/fds; Smithay builds typed `DmabufFeedback`. Only the input capability table/device id is equivalent. Wire ownership must stay with each protocol stack. |
| output topology | Legacy exposes one fixed output. Smithay owns output membership, xdg-output, migration and live scale. Constants can be shared, topology and event ordering cannot. |
| presentation completion | Both consume `PresentTiming`, but callback retention, discard and retry policies differ because Smithay has per-surface trees and bounded retained queues. Keep state machines local. |
| pointer dispatch/focus | Cocoa coordinate and button normalization is equivalent. Legacy adds window-geometry origin and writes raw protocol events; Smithay hit-tests surface trees, applies constraints, creates grabs and relative-pointer events. Sharing after normalization would bypass required Smithay policy. |
| viewport/logical sizing | Fixed-point floor/ceil in legacy and Smithay floating/typed geometry are not byte-equivalent at fractional boundaries. First write cross-path behavioral vectors; do not centralize until rounding policy is deliberately unified. |

## Sequenced maintenance plan

1. Land dmabuf constants/decoder and presentation constants/conversion first. These are cold, exact contracts
   and remove the highest-risk magic-number drift with no hot-path change.
2. Introduce the normalized input enum and switch both AppKit adapters in one commit. Preserve event order:
   motion before button, both scroll axes in one frame, and key modifier transitions.
3. Add cross-path transform golden vectors against the existing functions. Only then replace both with the
   checked shared implementation. Normal transform must return borrowed storage and invalid input must fail
   before allocation or partial output.
4. Extract the pixel blend kernel last. Verify rendered pixels for transparent, opaque, semi-transparent,
   clipped, scaled and cropped children on both paths. Inspect optimized assembly or use a micro measurement
   solely to demonstrate no new call/bounds-check regression.
5. Leave full compositor state machines independent until Smithay cutover. Centralizing divergent policy now
   would create conditionals and prolong the legacy architecture instead of reducing maintenance.

## Required acceptance evidence

- Rust unit tests for modifier encode/decode, device-id bytes, refresh conversion and all transform vectors.
- Rust rendered-pixel tests that compare pre/post extraction output, not source text or symbol presence.
- C or Rust Wayland clients capturing registry/dmabuf feedback and presentation behavior from each path.
- Rust host-input normalization vectors plus integration traces proving identical AppKit event ordering.
- `cargo test` for both compositor crates and optimized-code inspection for the two per-pixel helpers.

No benchmark suite is required or retained by this plan. Performance checks are temporary extraction evidence;
correctness remains defined by byte output, protocol traces and observable compositor behavior.
