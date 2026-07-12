# Rendering architecture: a reasonable path to a sharp, responsive, complete Wayland surface

Status: **research / plan only** (2026-07-11). No code changed by this document.

Goal (unchanged): run the Linux Chromium (Wayland client) inside the dd JIT container and
present it as a native macOS/Metal window that is **sharp** (native resolution, crystal
text), **responsive** (input propagates in <1 frame, cursor changes shape), and **complete**
(correct seat, scaling, subsurfaces).

The user's verdict on the current homegrown pipeline: content correctness is *achievable* but
the live experience is bad (blurry, multi-second input lag, cursor never changes) and the
approach is "fundamentally flawed." This document root-causes each symptom against the real
code, evaluates open-source components, and gives a prioritized, concrete plan.

---

## A) Root-cause table — each symptom → precise cause in the current code

| Symptom | Precise cause (file:line) | Confidence |
|---|---|---|
| **Blurry, not crystal-sharp** | The Retina present path is hardcoded to 1×. `MetalPresenter::make_window` sets `let scale = 1.0; layer.setContentsScale(1.0)` and `layer.setDrawableSize(w×h)` in **surface pixels**, and `window.setContentSize(w×h)` treating those pixels as **points** (`present_cocoa.rs:710-716`, repeated on resize at `:801-809`, and `surface_scale()` always returns `1.0` at `:1009`). On a 2× Retina panel a `CAMetalLayer` at `contentsScale=1` whose drawable is `w×h` covers a content view of `w×h` **points** = `2w×2h` **physical pixels**, so WindowServer bilinearly upscales the drawable 2×. Compounding it, `host_output_scale()` returns **1** unless `DD_DISPLAY_HIDPI` is set (`present_cocoa.rs:224-232`), so `wl_output.scale=1` is advertised (`server.rs:965,990`) and Chrome commits a **1× logical** buffer in the first place. Two independent 1× losses stacked. | **High** — read directly. |
| **Cursor never changes (no I-beam etc.)** | `wl_pointer.set_cursor` only records the surface as a cursor-role surface and drops its tiny window; the client's cursor image buffer is **discarded**, never turned into an `NSCursor` (`server.rs:1361-1369`). And `wp_cursor_shape_v1` is **not advertised** — it is absent from the `globals` list (`server.rs:855-872`; `grep` for `cursor_shape` across `dd-display/src` returns zero protocol code). So Chrome cannot use the themed named-cursor protocol, and its fallback (a `set_cursor` client buffer) is thrown away. The macOS arrow is never replaced. | **High** — read + grep. |
| **Input lag (click/select propagates after seconds)** | Input **delivery** is immediate — `pointer_motion`/`pointer_button`/`key`/`pointer_scroll` each end with `self.conn.flush()` (`server.rs:3161,3182,3238,3256`). The lag is **structural coupling**, not a protocol stall: (1) The single main-thread loop `run_multi` interleaves `poll(8ms) → pump(all clients) → drain NSEvents` (`present_cocoa.rs:1451-1554`); `pump()` runs `commit → present_root → present()` **synchronously**, and `present()` calls `nextDrawable()` which **blocks** on the swapchain (up to a vsync when active, historically ~1s when the app was background-throttled — the freeze the recent fix addressed at `:831-846`). Input NSEvents are only drained *after* `pump()` returns, so a slow present delays input handling. (2) **Frame-callback pacing**: `present_root` fires `wl_callback.done` + `wp_presentation.presented` **synchronously after** `present()` (`server.rs:1660-1666`), and Chrome/viz paces its next frame off them — so the guest runs one frame *behind* the present pipeline; anything that throttles present throttles Chrome's whole event loop, including its processing of the input bytes already on the socket. (3) Input is **polled** at 8ms in the same thread as present, never driven by an OS input callback or a display link — there is no decoupled immediate-input path. | **Medium-High** — delivery path read directly; the multi-second magnitude also involves guest/JIT scheduling, not readable from dd-display alone. |
| **General awkwardness / periodic multi-second lag** | Same present-coupling as above, **plus** the content hot-path is a hand-reimplemented GPU driver: Chrome GLES → `gl_shim.c` (GLSL→MSL translation) → `dd-gpu` IR → `metal_backend.rs` Metal replay → IOSurface. `metal_backend.rs` even **ignores the SPIR-V and ships one builtin vertex-color pipeline** (`metal_backend.rs:1-11`), and the replay path has produced a stream of correctness bugs (per-capture-variable Y-flip, glyph-atlas mirroring, mat3 uniform layout — see `docs/rendering/README.md §2-3` and recent git log). The **multi-process content-blank "Wall 7"** (`docs/bugs/README.md`) lives entirely in this replay stack's cross-process command-buffer/Mojo dependency. | **High** — read + existing docs. |

Net: the four symptoms are **not four independent bugs.** They are consequences of dd
hand-reimplementing **two** large systems that already exist as mature open source — a Wayland
compositor (`server.rs`, 4894 lines, missing fractional-scale, cursor-shape, and
subsurface-over-IOSurface compositing) and a **GPU driver** (`gl_shim` + `dd-gpu` +
`metal_backend`). Blur and cursor are cheap gaps in the first; the lag and the multi-process
wall are inherent to the second.

---

## B) Recommended architecture

### The pattern the field has converged on

Every product that presents a guest GUI as a *native, sharp, responsive* host window —
Parallels **Coherence**, VMware **Unity**, and the macOS project **cocoa-way** — uses the same
shape: **own the host window, receive per-surface buffers from the guest, composite them
natively.** They do *not* re-encode through a remote-desktop protocol (WSLg's RDP path pays a
VRAM→sysmem copy + encode/decode tax) and they do *not* reimplement a GPU driver.

dd already has the right *primitive* for the present half: a working **IOSurface↔Metal
zero-copy bridge** — `MetalCtx::texture_from_iosurface` uses
`newTextureWithDescriptor:iosurface:plane:` (`metal.rs:349-369`), fed by a **mach-port bridge**
(`dd_mach_server_start`/`dd_mach_recv`, `metal.rs:70-137`) that transfers IOSurface send-rights
from the engine to dd-display. What is wrong is the *source* of those pixels (a reimplemented
GPU driver) and the *finish* (1× present, no cursor, coupled input).

### Two concrete target architectures (choose by scope)

**Architecture 1 — Re-base the compositor on Smithay + Cocoa/Metal backend; feed Chrome via
wl_shm (recommended for generality).**

- **Smithay** (Rust Wayland-compositor library) replaces `server.rs`. Its protocol/state layer
  is platform-agnostic and ships maintained, spec-complete handlers for **xdg-shell, wl_seat,
  wp_viewporter, wp_fractional_scale_v1, wp_cursor_shape_v1, wp_presentation, zwp_linux_dmabuf**.
  Its Linux-only backends (`drm`/`libinput`/`udev`/`session`/`gbm`/`egl`) are **Cargo-feature-
  gated** and simply not enabled. There is **no monolithic `Backend` trait to satisfy**: you
  implement `InputBackend` (synthesized from `NSEvent`) and run your own present loop into a
  `CAMetalLayer`. The `Renderer`/`Frame` traits are graphics-agnostic (a pure-CPU
  `PixmanRenderer` exists in-tree), so a Metal renderer — or a straight IOSurface/texture blit —
  is pluggable. Sources: [Smithay repo](https://github.com/Smithay/smithay),
  [smithay::backend::input](https://docs.rs/smithay/latest/smithay/backend/input/index.html),
  [smithay::backend::renderer](https://docs.rs/smithay/latest/smithay/backend/renderer/index.html).
- **A direct precedent exists**: **cocoa-way** = Smithay core + Metal + Cocoa on macOS running
  real Wayland clients, with built-in **OrbStack** and `container run --publish-socket` modes.
  Its transport differs (waypipe from a separate Linux host rather than a local JIT container),
  and its docs are thin, but the *compositor half is identical to dd's target*. Sources:
  [J-x-Z/cocoa-way](https://github.com/J-x-Z/cocoa-way),
  [HN discussion](https://news.ycombinator.com/item?id=47553185).
- **Content via wl_shm, not GL→IR replay.** dmabuf/PRIME is impossible across the emulated-Linux
  → macOS boundary (Chrome makes **0 PRIME calls** here — `docs/rendering/README.md §3.2`).
  Leaving `zwp_linux_dmabuf_v1` unadvertised (already the default, `server.rs:876`) plus running
  Chrome with GPU compositing disabled makes Ozone/Wayland fall back to **`wl_shm`** — Chrome's
  renderer paints web content into browser-shared memory, which the compositor uploads **once**
  into a Metal/IOSurface texture per damage region. This **retires the reimplemented GPU driver
  on the content path** and, critically, **very likely dissolves the multi-process "Wall 7"**:
  the wall is the renderer→GPU-service command-buffer/Mojo channel; with shm raster there is no
  GPU-service channel to establish. This is the same route waypipe-on-macOS and WSLg use.
  Sources: [Chromium Ozone/Wayland (Igalia)](https://blogs.igalia.com/msisov/chrome-on-wayland-waylandification-project/),
  [Chromium Ozone overview](https://chromium.googlesource.com/chromium/src/+/HEAD/docs/ozone_overview.md).
- **Reuse from dd**: the `dd-jit` engine + Wayland socket plumbing; `MetalCtx` + the IOSurface
  mach bridge (`metal.rs`) for the present half; the keymap (`keymap.rs`, `kvk_to_evdev`); the
  NSWindow lifecycle + close/move/resize plumbing (`present_cocoa.rs`). **Replaced**: `server.rs`
  hand-written protocol; the `gl_shim`→`dd-gpu`→`metal_backend` content replay stack (kept only
  if a non-Chrome GLES client still needs it).

**Architecture 2 — CEF Offscreen Rendering (recommended only if Chromium is the *only* app).**

Drop the compositor, the Wayland seat plumbing, and the GL replay entirely. Embed **CEF**
(Chromium Embedded Framework) in windowless/OSR mode: `OnPaint` hands you CPU pixel buffers,
`OnAcceleratedPaint` hands you a **GPU shared-texture handle**, and you drive frame timing with
`SendExternalBeginFrame` and feed native macOS input/cursor/DPI straight into CEF — you own the
`NSWindow`, `NSCursor`, seat and scale. Lowest latency, least surface plumbing, but
**Chromium-only** and it changes what runs in the guest (a CEF build rather than stock Chrome).
Sources: [CEF](https://github.com/chromiumembedded/cef),
[CEF OSR shared-texture](https://github.com/chromiumembedded/cef/issues/3730).

### Why this beats incremental patching

The blur and cursor gaps are cheap to patch inside `server.rs`/`present_cocoa.rs` and **should
be** (see roadmap — they help immediately and carry over to either architecture). But the
lag-and-correctness class is *inherent to the reimplemented GPU driver*: the GL→IR→Metal replay
will keep generating orientation/glyph/shader-translation bugs, and the multi-process
content-blank wall is structurally in that stack. Switching Chrome to **wl_shm** deletes that
whole stack from the content path; adopting **Smithay** deletes the hand-written protocol state
machine (and hands you fractional-scale + cursor-shape for free). That is a net *reduction* in
dd-owned code on the hot path, which is why it is more reasonable than continuing to patch.

---

## C) Prioritized roadmap

Effort is rough calendar estimate for one engineer familiar with the code. "Reuse" vs "replace"
noted per item.

### Phase 0 — Quick, high-impact wins (do these first; they apply under *either* architecture)

**P0.1 — Finish the HiDPI / Retina present path (SHARPNESS). Highest leverage. ~2-4 days.**
- Advertise `wl_output.scale = 2` on Retina (make `host_output_scale` return
  `backingScaleFactor` by default rather than gating on `DD_DISPLAY_HIDPI`;
  `present_cocoa.rs:224-232`), so Chrome commits a **2× logical** buffer.
- In `MetalPresenter::make_window`/resize: set `layer.setContentsScale(2)`, `drawableSize =`
  **buffer pixels** (= 2×logical), and `window.setContentSize =` **logical points** (buffer÷2).
  Then 1 texel == 1 physical pixel — crystal sharp. (`present_cocoa.rs:710-716,801-809`.)
- Convert coordinates consistently: `flip_point` already takes a `scale` argument (currently
  passed `1.0` at `:1222,1629`); pass the real backing scale so `NSEvent` **points** map to
  surface **pixels**, and make `maybe_resize` request the **logical** size (window points), not
  raw points-as-pixels.
- **Do it properly with `wp_fractional_scale_v1` + `wp_viewporter`** for non-integer displays:
  advertise the fractional-scale global, send `preferred_scale` as 120ths (e.g. 180 for 1.5×),
  and let Chrome render at the fractional buffer size mapped by a viewport `destination`. Spec:
  [wp_fractional_scale_v1](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/fractional-scale/fractional-scale-v1.xml),
  [wp_viewporter](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/stable/viewporter/viewporter.xml).
- Apple side: [CAMetalLayer.contentsScale](https://developer.apple.com/documentation/quartzcore/cametallayer),
  [NSScreen.backingScaleFactor](https://developer.apple.com/documentation/appkit/nsscreen).
- **Reuse** existing present code; this is a completion, not a rewrite. Honest caveat: the
  points-vs-pixels cleanup touches input flip + xdg resize, so it is more than a one-liner.

**P0.2 — Cursor: `wp_cursor_shape_v1` + `set_cursor` → `NSCursor` (CURSOR CHANGES). ~2-3 days.**
- Advertise `wp_cursor_shape_manager_v1`; handle `wp_cursor_shape_device_v1.set_shape(serial,
  shape)` and map the shape enum to `NSCursor`. Spec:
  [wp_cursor_shape_v1](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/staging/cursor-shape/cursor-shape-v1.xml).
  Mapping table (canonical shapes → AppKit):

  | Wayland shape | NSCursor |
  |---|---|
  | `default` | `arrowCursor` |
  | `text`, `vertical_text` | `iBeamCursor`, `iBeamCursorForVerticalLayout` |
  | `pointer` | `pointingHandCursor` |
  | `crosshair`, `cell` | `crosshairCursor` |
  | `grab` / `grabbing` | `openHandCursor` / `closedHandCursor` |
  | `not_allowed`, `no_drop` | `operationNotAllowedCursor` |
  | `ew-resize`, `col-resize`, `e/w-resize` | `resizeLeftRightCursor` |
  | `ns-resize`, `row-resize`, `n/s-resize` | `resizeUpDownCursor` |
  | `copy` | `dragCopyCursor` |
  | `alias` | `dragLinkCursor` |
  | `context_menu`, `help`, `progress`, `wait`, `all_scroll`, `zoom_in/out`, diagonal resizes | best-effort (`arrowCursor`/`crosshairCursor`; no exact NSCursor) |

  ([NSCursor](https://developer.apple.com/documentation/appkit/nscursor).)
- Also honor the classic `wl_pointer.set_cursor` client-buffer path as a fallback: instead of
  discarding the image (`server.rs:1361-1369`), either hide the system cursor and draw the
  client buffer, or build an `NSCursor` from it. cursor-shape covers Chrome's common case;
  set_cursor covers custom/CSS cursors.
- Add a `Presenter::set_cursor(shape)` hook to the trait (`present.rs:54-108`) so the platform
  layer sets the `NSCursor` on the main thread.

**P0.3 — Decouple input from present pacing (RESPONSIVENESS). ~3-5 days.**
- Drain and inject `NSEvent`s on their **own cadence**, not gated behind `pump()`/`present()`.
  Options: move present onto a `CVDisplayLink`/`CADisplayLink` callback (present at vsync,
  independent of the poll loop), and drain input every loop turn before doing any present work;
  or register the client fd + an input source with a `CFRunLoopSource` so input wakes the loop
  immediately. ([CVDisplayLink](https://developer.apple.com/documentation/corevideo/cvdisplaylink),
  [CADisplayLink on macOS 14+](https://developer.apple.com/documentation/quartzcore/cadisplaylink).)
- Never let `nextDrawable()` block the input path — keep the "composite offscreen when inactive"
  fix (`present_cocoa.rs:831-846`) and additionally bound drawable acquisition when active.
- **Reuse** `inject_nsevent`/`route_input`; this is a loop-structure change.

Phase 0 alone should make the window **sharp**, give it a **changing cursor**, and cut
click-to-response to roughly one frame — directly addressing the three loudest complaints —
*without* the larger structural move.

### Phase 1 — Structural: retire the reimplemented GPU driver on the content path

**P1.1 — Switch Chrome content to wl_shm. ~1-2 weeks (mostly bring-up + validation).**
- Run Chrome (Ozone/Wayland) with GPU compositing disabled so web content rasterizes into
  `wl_shm`; keep `zwp_linux_dmabuf` unadvertised. The existing shm→Metal upload present path
  (`present()` `None` arm, `upload_bgra`, `present_cocoa.rs:776`) already handles shm buffers.
- Expected payoffs: (a) eliminates the offscreen-tile Y-flip / glyph-mirror / shader-translation
  bug class; (b) **very likely resolves multi-process Wall 7** (no GPU-service command-buffer
  channel to establish); (c) far simpler hot path. Cost: CPU raster is slower than GPU on heavy
  pages, and one memcpy + one texture upload per damaged region per frame — acceptable at browser
  resolutions (this is exactly what WSLg/waypipe-on-macOS run). Sources:
  [WSLg architecture](https://devblogs.microsoft.com/commandline/wslg-architecture/),
  [waypipe design](https://mstoeckl.com/notes/gsoc/blog.html),
  [waypipe-darwin (macOS shm)](https://github.com/J-x-Z/waypipe-darwin).
- Honest risk: some Chrome builds insist on GPU compositing for certain content; validate that
  the shm fallback is stable and performant for the target pages before committing.
- **Reuse** shm present path + IOSurface bridge; **replace** the content-path use of
  `gl_shim`/`dd-gpu`/`metal_backend` (retain them for any non-Chrome accelerated client).

### Phase 2 — Structural: re-base the compositor (largest, optional)

**P2.1 — Adopt Smithay with a Cocoa/Metal backend. ~4-8 weeks; medium-high risk.**
- Replace `server.rs` with Smithay's protocol state + handlers, a custom `InputBackend` from
  `NSEvent`, and a Metal/IOSurface present loop. Gains complete, maintained fractional-scale,
  cursor-shape, viewporter, seat, subsurface, and presentation handling; deletes ~5k lines of
  hand-written protocol. Mirror **cocoa-way**'s structure.
- Risk/blockers to be honest about: Smithay assumes a Linux-ish build in places (feature-gate
  audit needed to keep only platform-agnostic crates); the `Renderer` integration for a
  Metal/IOSurface output is bespoke; cocoa-way's transport differs and its internals are
  under-documented, so it is a reference, not a drop-in. This phase is only worth it once Phase 0
  + P1.1 have proven the shm-content + native-present model.
- **Alternative to P2.1**: if scope narrows to "a browser in a native Mac window," **CEF OSR**
  (Architecture 2) reaches the same sharp/low-latency/complete goal with less total code — at the
  cost of Chromium-only and a CEF build in the guest.

---

## D) The single highest-leverage change to do FIRST — and how to validate it

**Do first: P0.1 — finish the HiDPI / Retina present path (contentsScale=2 + native-pixel
drawable + `wl_output.scale=2` so Chrome renders a 2× buffer).**

Why first: it is the direct cause of the #1 complaint (blur), it is small and self-contained,
and it is required under every downstream architecture. It changes the experience from "renders
but soft" to "crystal sharp" in one contained change.

How to validate (the user judges on-screen, so validate visually + with the existing debug
hooks):
1. **Sharpness (eyes):** launch Chrome on the live path; page text and UI must be crisp, with no
   soft bilinear halo. Compare a screenshot before/after at 100%.
2. **Instrumented check:** enable `DD_DISPLAY_PRESENT_DEBUG=changes`
   (`present_cocoa.rs:490-584`) and confirm `layer_drawable` and `drawable_tex` are **2×** the
   logical window points (e.g. a 1000×700-pt window shows a 2000×1400 drawable), and that the
   committed Chrome buffer (`surf`/`texture` size in the same log) is 2× logical — proving Chrome
   rendered a retina buffer *and* it presents 1:1.
3. **Regression:** run `run_golden.sh` (Metal IR-replay pixel-diff) and the
   `dd-display` lib tests to confirm the coordinate/scale cleanup did not break the 1× headless
   path.
4. Then validate P0.2 (cursor turns to I-beam over text, pointer over links) and P0.3 (select
   text / click responds within ~1 frame) the same way — by eye, with `DD_DISPLAY_INPUT_DEBUG`
   confirming events route on the immediate path.

---

## Appendix — components evaluated

- **Smithay** (adopt, Phase 2) — Rust compositor library, platform-agnostic core, feature-gated
  Linux backends, ships fractional-scale + cursor-shape + viewporter + seat. Precedent:
  cocoa-way. [repo](https://github.com/Smithay/smithay) ·
  [docs](https://docs.rs/smithay/latest/smithay/).
- **wlroots** (reject) — C; DRM/GBM/libinput/logind pervade its buffer model
  (`get_drm_fd` in the backend vtable assumes a DRM device); no macOS port.
  [repo](https://github.com/swaywm/wlroots).
- **cocoa-way** (blueprint) — Smithay + Metal + Cocoa on macOS, OrbStack-aware.
  [repo](https://github.com/J-x-Z/cocoa-way) · [HN](https://news.ycombinator.com/item?id=47553185).
- **waypipe / waypipe-darwin** (transport reference) — proven Wayland-over-a-boundary proxy;
  shm diff transport; macOS fork exists (shm-only, no dmabuf).
  [design](https://mstoeckl.com/notes/gsoc/blog.html) ·
  [darwin fork](https://github.com/J-x-Z/waypipe-darwin).
- **WSLg** (lesson: shared-buffer, not RDP re-encode) —
  [architecture](https://devblogs.microsoft.com/commandline/wslg-architecture/).
- **CEF OSR** (alternative Architecture 2) — [repo](https://github.com/chromiumembedded/cef).
- **IOSurface↔Metal** (already in dd) — `newTextureWithDescriptor:iosurface:plane:`
  (`metal.rs:349-369`); mach-port bridge (`metal.rs:70-137`). Apple:
  [MTLDevice.makeTexture(descriptor:iosurface:plane:)](https://developer.apple.com/documentation/metal/mtldevice),
  [IOSurface](https://developer.apple.com/documentation/iosurface).
- **Chromium Ozone/Wayland** — [Igalia Waylandification](https://blogs.igalia.com/msisov/chrome-on-wayland-waylandification-project/).
