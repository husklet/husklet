# Smithay + wgpu default-readiness

Status: **audit, 2026-07-12.** Can `DD_DISPLAY_SMITHAY=1` (the Smithay-native `dd-compositor`) and
`DD_GPU_BACKEND=wgpu` (the wgpu host GPU executor) become the DEFAULT, keeping the working legacy
Chrome/GTK render as a fallback?

**Verdict: not yet — but the protocol surface is a superset of the legacy path and all unit/integration
tests pass.** The blockers are (1) `zwp_linux_dmabuf` **v4 feedback** for the accelerated Chromium GPU
path, (2) popups composited-into-parent (clipped) vs. legacy native popup windows, (3) neither crate is
in any build gate — `dd-compositor` was in fact **broken on `main`** (fixed here), (4) `libxkbcommon`
is not bundled into `dd.app`, (5) no live Chrome/GTK-on-Smithay validation has been run. Items 1, 3, 4,
5 gate a safe flip; 2 is a visible-quality regression to close or accept.

This document is a source-read + unit-test + build audit. No live Chrome/GTK session was run (that is
the coordinated validation step, driven separately).

---

## 1. How the two flags are consumed (where to flip, and the escape hatch)

- **`DD_DISPLAY_SMITHAY`** — read once in `dd-display/src/main.rs::maybe_exec_smithay()` (line ~408).
  On `"1"|"true"|"on"` it `exec`s the sibling `dd-compositor` binary in place, forwarding argv; on
  anything else it falls through to the legacy `server.rs` path. `dd-display` never links smithay — it
  is an exec boundary, so the legacy binary stays smithay-free.
  *To flip:* invert the match so the compositor is the default target and an explicit
  `DD_DISPLAY_SMITHAY=0|legacy|off` forces `server.rs`. The exec already logs + falls back to legacy if
  `dd-compositor` is missing or `exec` fails, so a broken/absent compositor cannot dark-screen the
  display — a good safety property to KEEP when flipping.

- **`DD_GPU_BACKEND`** — read in `dd_gpu_wgpu::selected()` (`dd-gpu-wgpu/src/lib.rs`), consumed in
  `dd-display/src/metal_backend.rs::run_executor()` (line ~1204): when `=="wgpu"` it dispatches to
  `run_executor_wgpu()` (wgpu over dd's process-shared `MTLDevice`, zero-copy into the same IOSurface
  the compositor blits, fenced with the same `MTLEvent`s), else the bespoke `MetalBackend` replay.
  *To flip:* default `selected()` to wgpu unless `DD_GPU_BACKEND=metal|legacy`.

The two flags are **independent**: Smithay-vs-legacy is the *compositor*; wgpu-vs-metal is the *GPU IR
executor*. Either can be flipped without the other.

---

## 2. Protocol / feature matrix — legacy `server.rs` vs `dd-compositor`

Legacy globals enumerated from `dd-display/src/server.rs`; Smithay coverage from
`dd-compositor/src/lib.rs` (`DdState::new` + `delegate_*!`) and `handlers/`. "superset" = Smithay
advertises it and legacy does not.

| Protocol / feature | Legacy `server.rs` | `dd-compositor` (Smithay) | Notes |
|---|---|---|---|
| `wl_compositor` / `wl_subcompositor` | yes | **yes** (v5) | |
| `wl_shm` (CPU raster) | yes | **yes** (ARGB/XRGB8888) | commit→repack→present, damage-tracked |
| Subsurfaces (sync/desync, nesting) | yes | **yes** | CPU over-composite into the toplevel frame (`present_render_root`/`blend_subtree`) |
| `xdg_wm_base` toplevel (configure/ack, min/max, maximize/fullscreen/minimize, close) | yes | **yes** | |
| `xdg_wm_base` popups (positioner, constrain flip/slide, grab, reposition v3) | yes | **yes** | **but** popups are composited into the parent frame and **clipped to parent bounds** — legacy opens a native popup window at the anchor (`SurfaceBuffer::popup`). See Gap 2. |
| `xdg_toplevel.move` / `.resize` interactive grabs | yes | **yes** | serial-validated against recent input; drives native NSWindow move/resize via `Presenter` |
| `wl_seat` keyboard (XKB) + pointer | yes | **yes** (v5) | libxkbcommon keymap; full kVK→evdev map + modifier/CapsLock bridge |
| `wl_touch` | declared, no device | declared (`TouchFocus`), no device | parity — neither wires a real touch device |
| `wl_pointer.set_cursor` (client bitmap cursor) | legacy discarded it (per ARCHITECTURE-PLAN) | **yes** | `CursorImageStatus::Surface` → `set_cursor_buffer` NSCursor |
| `wp_cursor_shape_v1` (themed cursors) | **no** (absent) | **yes** (superset) | `CursorIcon`→wp-shape→NSCursor |
| `wl_output` | yes (v4) | **yes** (v4) | HiDPI scale from `Presenter::output_scale()` |
| `xdg_output` (logical geometry/name) | **no** | **yes** (superset) | multi-output-ready (`add_output`) |
| `wp_viewporter` (src crop / dst size) | yes | **yes** | shared `logical_size_and_uv` for shm + dmabuf |
| `wp_presentation` (feedback, MSC) | yes | **yes** | Linux `CLOCK_MONOTONIC`(1); presented/discarded per commit |
| `zwp_linux_dmabuf` (GPU/IOSurface present) | **v4 + feedback** (`DD_DISPLAY_DMABUF`) | **v3 only** | dd IOSurface-modifier bridge present; **v4 feedback missing** — see Gap 1 |
| `wl_data_device` (clipboard + DnD) | yes | **yes** | Smithay-driven guest↔guest + host `NSPasteboard` bridge both ways |
| `zwp_primary_selection_v1` (middle-click) | **no** | **yes** (superset) | |
| `wp_fractional_scale_v1` (non-integer HiDPI) | **no** | **yes** (superset) | `DD_DISPLAY_FRACTIONAL_SCALE` override |
| `wp_single_pixel_buffer_v1` | **no** | **yes** (superset) | |
| `xdg_decoration` (SSD/CSD negotiation) | **no** | **yes** (superset) | CSD default; SSD under `DD_DISPLAY_WINDOW_DECORATIONS` |
| `xdg_activation_v1` (focus/raise) | **no** | **yes** (superset) | |
| `zwp_relative_pointer_v1` (games/3D) | **no** | **yes** (superset) | |
| `zwp_pointer_constraints_v1` (lock/confine) | **no** | **yes** (superset) | cursor-hide synced to lock |
| `zwp_text_input_v3` (IME / marked text) | **no** | **yes** (superset) | compositor IS the input method (host macOS IME bridge) |
| `surface_augmenter` (ChromeOS exo) | off by default (`DD_DISPLAY_AUGMENTER`) | n/a | not needed (stock Weston never advertises it) |

**Bottom line:** for the **software (`wl_shm`) render** — the path that today renders Chrome/GTK4 UI
live on legacy — `dd-compositor` is a strict **superset** of legacy. The only global where legacy is
ahead is `zwp_linux_dmabuf` **v4 feedback**, which gates the *accelerated* Chromium GPU bring-up.

---

## 3. Test results (mac toolchain, this worktree)

Built + tested via the `mac` bridge with
`RUSTFLAGS="-L native=<nix libxkbcommon>/lib"`, `DYLD_LIBRARY_PATH=<same>/lib`, isolated
`CARGO_TARGET_DIR`.

- `cargo build -p dd-compositor` — **green** (only pre-existing `dd-display` warnings).
- `cargo test -p dd-compositor` — **5/5 pass**:
  - `client_roundtrip::globals_advertise_frame_presents_feedback_and_cursor_shape_wire`
  - `dmabuf_present::dmabuf_global_and_iosurface_commit_presents`
  - `robustness::compositor_survives_stress_disconnect_and_bogus_requests`
  - `scale_outputs::fractional_scale_xdg_output_single_pixel_and_multi_output`
  - `text_input::text_input_focus_enable_and_commit_string_roundtrip`
- `cargo test -p dd-gpu-wgpu` — **3/3 pass** (`shader::` GLSL/SPIR-V→WGSL lowering). These are
  translation-only; the device-level examples (`iosurface_interop`, `verify_ir`) require a live Metal
  device and are **not** run as tests → the wgpu executor's end-to-end IOSurface render is **untested
  in CI**.

### Build break found and fixed on `main`

Commit `48f9bfe1` added `SurfaceBuffer::popup: Option<PopupPlacement>` to `dd-display/src/present.rs`
and updated both `server.rs` construction sites (`popup: None`) — but **not** `dd-compositor`'s two
sites (`handlers/dmabuf.rs`, `handlers/compositor.rs`). Because `dd-compositor` is **not** in
`default-members` and is built only on the mac, bare `cargo build` never caught it: **`dd-compositor`
did not compile on `main`.** Fixed here by adding `popup: None` to both sites (the compositor composites
popups into the parent frame, so no native popup placement is emitted — the correct value for its
model). This is exactly the "scoped agents miss cross-cutting regressions" trap: a `dd-display` struct
change silently broke the un-gated compositor.

---

## 4. Gaps blocking a safe default flip (ordered)

1. **`zwp_linux_dmabuf` v4 feedback (accelerated Chromium).** `dd-compositor` advertises the v3 global
   only. Chromium's ozone/GPU derives its DRM render-node path from the dmabuf-feedback `main_device`
   via `get_default_feedback` (legacy `server.rs` implements this at ~line 2500). Without it,
   GPU-composited Chrome cannot bring up its accelerated path on Smithay and falls back to `wl_shm`.
   The compositor deliberately used v3 because Smithay's v4 feedback builds its format table in a
   `shm_open`ed file named `smithay-dmabuffeedback-format-table` (35 chars) that exceeds macOS
   `PSHMNAMLEN` (31) → `ENAMETOOLONG`. **Fixing this is NOT low-risk** (upstream Smithay behaviour /
   a shim of the format-table path) and is left for the coordinated GPU step. GLES clients
   (glmark2/es2tri) that read only the v3 modifier list are unaffected.

2. **Popups clip to the parent frame.** `dd-compositor` CPU-composites popups (and subsurfaces) into
   the toplevel's frame and clips them to the parent bounds (`blend()` documents this). Legacy opens a
   native popup window at the positioner anchor via `SurfaceBuffer::popup`/`PopupPlacement`. A menu or
   dropdown that extends past the window edge will be **clipped** on Smithay. Close it by having the
   compositor emit `SurfaceBuffer::popup` for `xdg_popup` roots (the platform seam already supports it),
   or accept it as a known limitation for the first flip. Medium effort; not required for correctness of
   in-window menus.

3. **Neither crate is in a build gate.** `dd-compositor` and `dd-gpu-wgpu` appear in **no** Makefile /
   nix / CI target (`grep` is empty), which is how #3's break landed. **Before flipping**, add at least
   a mac `cargo build -p dd-compositor -p dd-gpu-wgpu` (+ `cargo test`) to the release/gate flow so the
   default path cannot silently rot.

4. **`libxkbcommon` is not bundled into `dd.app`.** Smithay links it unconditionally at
   link+run time; there is no `libxkbcommon` reference in `Makefile`/`nix`. An end-user `dd.app` running
   the compositor by default would fail to launch. Bundle `libxkbcommon.dylib` in
   `dd.app/Contents/Frameworks` with `-rpath @executable_path/../Frameworks` (or static-link) as part of
   the flip. Until then the exec-fallback (§1) would silently route everyone back to legacy — masking the
   flip.

5. **No live Chrome/GTK-on-Smithay validation.** All green results here are unit/integration
   (`PngPresenter`, headless). The live NSWindow present/input loop (`main.rs::macos`), the
   Cocoa/Metal presenter path, HiDPI sharpness, and real Chrome/GTK4 bring-up have **not** been
   exercised on Smithay. This is the coordinated validation step and is the true gate on flipping.

Non-blocking parity notes: touch (neither path has a real device); DnD drag-icon surface (host cursor
only, both paths).

---

## 5. Safe flip plan (for the later coordinated step — NOT done here)

1. Land the build gate (Gap 3) and `libxkbcommon` bundling (Gap 4) FIRST, so the compositor is actually
   built, shipped, and launchable before it becomes default.
2. Run live Chrome + GTK4 on `DD_DISPLAY_SMITHAY=1` (software `wl_shm` path) and confirm UI parity
   (sharpness, input latency, cursor shape, menus, clipboard) against the legacy baseline. The `wl_shm`
   path needs none of Gap 1.
3. Decide Gap 2 (emit native popup placement, or accept clipping) based on what the live menus show.
4. For the accelerated path, resolve Gap 1 (dmabuf v4 feedback) and validate GPU-composited Chrome; keep
   `DD_GPU_BACKEND=wgpu` behind its own flip until the wgpu executor has a device-level test or a live
   run.
5. Flip the default in `maybe_exec_smithay()` (compositor default; `DD_DISPLAY_SMITHAY=0` = legacy
   escape hatch) and, separately, `dd_gpu_wgpu::selected()` (wgpu default; `DD_GPU_BACKEND=metal` =
   legacy escape hatch). Preserve the exec-fallback-to-legacy safety net.

Escape hatches after the flip: `DD_DISPLAY_SMITHAY=0` → legacy `server.rs`; `DD_GPU_BACKEND=metal` →
bespoke Metal replay. Both are single-env, no rebuild.

---

## 6. Changes made by this audit (low-risk only)

- Fixed the `dd-compositor` build break on `main`: added `popup: None` to the `SurfaceBuffer`
  constructions in `dd-compositor/src/handlers/dmabuf.rs` and `handlers/compositor.rs` (see §3). No
  behaviour change — the compositor's popup-into-parent model does not use native popup placement.

Explicitly NOT done (out of low-risk scope, per the task): flipping either default; dmabuf v4 feedback;
native popup windows; the build-gate/bundling wiring; any live Chrome/GTK run.
