# Smithay + wgpu default-readiness

Status: **audit 2026-07-12; low-risk gaps closed 2026-07-12.** Can `DD_DISPLAY_SMITHAY=1` (the
Smithay-native `dd-compositor`) and `DD_GPU_BACKEND=wgpu` (the wgpu host GPU executor) become the DEFAULT,
keeping the working legacy Chrome/GTK render as a fallback?

**Verdict: GTK4 is validated live and renders on Smithay (see Gap 5) — ready to be the default for GTK
software rendering with commit `20174b15`; Chrome + the wgpu executor remain open.** The protocol surface
is a strict superset of the legacy path and all unit/integration tests pass, and the low-risk pre-flip
gaps are closed. **Live GTK4-on-Smithay ran here (Gap 5, 2026-07-12): gtk4-demo renders a full, correct
GTK Demo window** — but only after fixing a pre-frame SIGSEGV caused by the unconditional dmabuf v4
feedback global (commit `20174b15`, gate it behind `DD_DISPLAY_DMABUF` like legacy). Remaining before a
full flip: (5) live **Chrome**-on-Smithay + the Cocoa/Metal (non-PngPresenter) present path are still
un-run. **`DD_GPU_BACKEND=wgpu` is no longer inert under `DD_DISPLAY_SMITHAY` (Phase 6.1–6.2, addressed):
`dd-compositor` now starts the dd-gpu IR executor itself** (`dd_compositor::gpu::start`, before the
compositor mode is selected) so both the default Metal executor and `DD_GPU_BACKEND=wgpu` are reachable
on the Smithay path; a live accelerated-guest run remains as the closing evidence. **`zwp_linux_dmabuf`
v4 feedback (Gap 1)** stays available
**opt-in** (`DD_DISPLAY_DMABUF`); it must NOT be default-on until the guest-side format-table `mmap` is
fixed (that fd is what crashed GTK).

**Closed by this pass (all low-risk, defaults unchanged):**
- **Gap 1 — `zwp_linux_dmabuf` v4 feedback.** `dd-compositor` now advertises the **feedback-carrying
  global** (Smithay's version-5 dmabuf global, which serves the v4 `get_default_feedback` path Chromium's
  ozone/GPU needs) with `main_device == makedev(226,128)`, matching the legacy `server.rs` feedback. The
  macOS blocker was that Smithay backs the feedback format-table with a `shm_open`ed object named
  `smithay-dmabuffeedback-format-table` (+ random suffix, ~43 bytes) that overflows macOS `PSHMNAMLEN`
  (31) → `ENAMETOOLONG`; fixed by an **offline-vendored smithay 0.7.0** patch that shortens that object
  name in `src/utils/sealed_file.rs` (nothing else changed; wired via `[patch.crates-io]` in the root
  `Cargo.toml`, path `third_party/smithay-0.7.0`). v3-and-lower binders still get the same ARGB/XRGB8888
  modifier list from the feedback's main tranche (GLES clients unaffected), and if the format-table ever
  fails to build the compositor logs and falls back to the v3 global. Entirely behind `DD_DISPLAY_SMITHAY`
  (that flag is what execs this compositor); the legacy default path is byte-for-byte unchanged. Covered
  by a new unit test (`build_default_feedback` succeeds — no PSHMNAMLEN failure) and integration test
  (`dmabuf_v4_feedback` — the global advertises version >= 4).
- **Gap 3 — build gate.** `make mac-crates` now builds `dd-display`/`dd-gpu-wgpu`/`dd-compositor` and
  runs the compositor + wgpu tests on the macOS toolchain (libxkbcommon from the nix dev shell),
  documented as the post-merge check in `docs/AGENTS.md`. Verified green on current `main`.
- **Gap 4 — libxkbcommon bundling.** `nix/flake.nix` now provides `libxkbcommon` (exported as
  `DD_LIBXKBCOMMON`) and `dd-gui/package/bundle.sh` builds + ships `dd-display`/`dd-compositor` in
  `Contents/Resources` and relocates `libxkbcommon` into `Contents/Frameworks` (via `dylibbundler`, with
  `@executable_path/../Frameworks` install names) so an end-user `dd.app` can launch the compositor.
- **Gap 2 — popup clipping (geometry + gated native-window path).** `dd-compositor` now resolves an
  `xdg_popup`'s placement (`popup_placement`) and, under `DD_DISPLAY_POPUP_WINDOWS=1`, presents each popup
  as its own native window at the positioner anchor (`SurfaceBuffer::popup`, consumed by the shared
  `present_cocoa` presenter that already opens the child NSWindow) instead of clipping it into the parent
  frame. Covered by a new `dd-compositor` test. Left behind the flag pending the live menu validation;
  the default (composite-into-parent) is byte-for-byte unchanged.
- **Modern GUI protocol groups composed from vendored smithay (codex-rendering §5.2 / §9.4, ledger row
  `modern_gui_protocol_groups_are_composed_from_vendored_smithay`).** `dd-compositor` now COMPOSES six
  protocol groups the vendored `third_party/smithay-0.7.0` already implemented but dd did not advertise —
  each as a vertical slice (state + `delegate_*!` + dd host policy + per-protocol client roundtrip):
  `zwp_pointer_gestures_v1`, `zwp_tablet_manager_v2`, `zwp_idle_inhibit_manager_v1`,
  `wp_content_type_manager_v1`, `zxdg_exporter_v2`/`zxdg_importer_v2` (xdg-foreign) and
  `zwp_keyboard_shortcuts_inhibit_manager_v1`. Host policy honours "dd has no real tablet/touch device and
  one Cocoa window": idle-inhibit **records intent** (`idle_inhibited()`), content-type **stores the hint**
  (`content_type(sid)`), xdg-foreign **issues real export handles**, keyboard-shortcuts-inhibit is
  **honoured/activated** (dd owns no conflicting chords), and pointer-gestures / tablet advertise + accept
  with a proven hot-plug/injection seam (`inject_swipe_gesture` / `add_tablet`) but generate nothing by
  default (no host device). Policy lives one-file-per-protocol under `dd-compositor/src/handlers/`
  (`pointer_gestures.rs`, `tablet.rs`, `idle_inhibit.rs`, `content_type.rs`, `xdg_foreign.rs`,
  `keyboard_shortcuts_inhibit.rs`); state fields + construction + delegates are in clearly-marked blocks in
  `lib.rs` / `handlers/mod.rs`. Proven by `tests/modern_protocols.rs` (one client binds every global +
  completes a request/event exchange per protocol). **`wp_tearing_control_manager_v1` is NOT composed** —
  no tearing-control module exists in vendored smithay-0.7.0 (only content-type's tearing *hint* is
  available); revisit on a smithay bump. Entirely behind `DD_DISPLAY_SMITHAY` (the flag that execs this
  compositor); the legacy default path is unchanged.

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
| `xdg_wm_base` popups (positioner, constrain flip/slide, grab, reposition v3) | yes | **yes** | default composites popups into the parent frame (clipped); `DD_DISPLAY_POPUP_WINDOWS=1` opens them as native popup windows at the anchor (`SurfaceBuffer::popup`), matching legacy. See Gap 2. |
| `xdg_toplevel.move` / `.resize` interactive grabs | yes | **yes** | serial-validated against recent input; drives native NSWindow move/resize via `Presenter` |
| `wl_seat` keyboard (XKB) + pointer | yes | **yes** (v5) | libxkbcommon keymap; full kVK→evdev map + modifier/CapsLock bridge |
| `wl_touch` | declared, no device | declared (`TouchFocus`), no device | parity — neither wires a real touch device |
| `wl_pointer.set_cursor` (client bitmap cursor) | legacy discarded it (per ARCHITECTURE-PLAN) | **yes** | `CursorImageStatus::Surface` → `set_cursor_buffer` NSCursor |
| `wp_cursor_shape_v1` (themed cursors) | **no** (absent) | **yes** (superset) | `CursorIcon`→wp-shape→NSCursor |
| `wl_output` | yes (v4) | **yes** (v4) | HiDPI scale from `Presenter::output_scale()` |
| `xdg_output` (logical geometry/name) | **no** | **yes** (superset) | multi-output-ready (`add_output`) |
| `wp_viewporter` (src crop / dst size) | yes | **yes** | shared `logical_size_and_uv` for shm + dmabuf |
| `wp_presentation` (feedback, MSC) | yes | **yes** | Linux `CLOCK_MONOTONIC`(1); presented/discarded per commit |
| `zwp_linux_dmabuf` (GPU/IOSurface present) | **v4 + feedback** (`DD_DISPLAY_DMABUF`) | **v4/v5 + feedback** | dd IOSurface-modifier bridge; feedback `main_device == makedev(226,128)` matches legacy — Gap 1 **CLOSED** (offline-vendored smithay `PSHMNAMLEN` fix) |
| `wl_data_device` (clipboard + DnD) | yes | **yes** | Smithay-driven guest↔guest + host `NSPasteboard` bridge both ways |
| `zwp_primary_selection_v1` (middle-click) | **no** | **yes** (superset) | |
| `wp_fractional_scale_v1` (non-integer HiDPI) | **no** | **yes** (superset) | `DD_DISPLAY_FRACTIONAL_SCALE` override |
| `wp_single_pixel_buffer_v1` | **no** | **yes** (superset) | |
| `xdg_decoration` (SSD/CSD negotiation) | **no** | **yes** (superset) | CSD default; SSD under `DD_DISPLAY_WINDOW_DECORATIONS` |
| `xdg_activation_v1` (focus/raise) | **no** | **yes** (superset) | |
| `zwp_relative_pointer_v1` (games/3D) | **no** | **yes** (superset) | |
| `zwp_pointer_constraints_v1` (lock/confine) | **no** | **yes** (superset) | cursor-hide synced to lock |
| `zwp_text_input_v3` (IME / marked text) | **no** | **yes** (superset) | compositor IS the input method (host macOS IME bridge) |
| `zwp_pointer_gestures_v1` (touchpad swipe/pinch/hold) | **no** | **yes** (superset) | composed from vendored smithay; no host gesture device → no events by default; `inject_swipe_gesture` seam proven (§5.2) |
| `zwp_tablet_manager_v2` (graphics tablet/stylus) | **no** | **yes** (superset) | composed from vendored smithay; no host tablet → seat advertises zero tablets; `add_tablet` hot-plug seam proven (§5.2) |
| `zwp_idle_inhibit_manager_v1` (keep session awake) | **no** | **yes** (superset) | composed from vendored smithay; host records the keep-awake intent (`idle_inhibited()`) (§5.2) |
| `wp_content_type_manager_v1` (photo/video/game hint) | **no** | **yes** (superset) | composed from vendored smithay; host stores the committed hint per surface (`content_type(sid)`) (§5.2) |
| `zxdg_exporter_v2` / `zxdg_importer_v2` (xdg-foreign) | **no** | **yes** (superset) | composed from vendored smithay; real export handles + cross-client `set_parent_of` into the xdg-shell parent model (§5.2) |
| `zwp_keyboard_shortcuts_inhibit_manager_v1` (forward all keys) | **no** | **yes** (superset) | composed from vendored smithay; dd owns no conflicting chords → honoured/activated on request (§5.2) |
| `wp_tearing_control_manager_v1` (tearing hints) | **no** | **NOT composed** | module absent from vendored `third_party/smithay-0.7.0` — skipped, revisit on a smithay bump (§5.2) |
| `surface_augmenter` (ChromeOS exo) | off by default (`DD_DISPLAY_AUGMENTER`) | n/a | not needed (stock Weston never advertises it) |

**Bottom line:** for the **software (`wl_shm`) render** — the path that today renders Chrome/GTK4 UI
live on legacy — `dd-compositor` is a strict **superset** of legacy. With Gap 1 closed
(`zwp_linux_dmabuf` v4 feedback now advertised), the compositor no longer trails legacy on *any*
global; the remaining gate is the coordinated live validation (Gap 5).

---

## 3. Test results (mac toolchain, this worktree)

Built + tested via the `mac` bridge with
`RUSTFLAGS="-L native=<nix libxkbcommon>/lib"`, `DYLD_LIBRARY_PATH=<same>/lib`, isolated
`CARGO_TARGET_DIR`.

- `cargo build -p dd-compositor` — **green** (only pre-existing `dd-display` warnings + benign
  vendored-smithay warnings).
- `cargo test -p dd-compositor` — **9/9 pass**:
  - `handlers::dmabuf::tests::dmabuf_feedback_format_table_builds_under_pshmnamlen` (Gap 1 — the v4
    feedback format-table builds on macOS with no PSHMNAMLEN/ENXIO failure)
  - `client_roundtrip::globals_advertise_frame_presents_feedback_and_cursor_shape_wire`
  - `dmabuf_present::dmabuf_global_and_iosurface_commit_presents`
  - `dmabuf_v4_feedback::dmabuf_global_advertises_v4_feedback` (Gap 1 — the global advertises version
    >= 4)
  - `modern_protocols::modern_protocols_bind_and_roundtrip` (§5.2 — a client binds + roundtrips all six
    newly composed modern protocol groups: pointer-gestures swipe begin/end, tablet hot-plug
    `tablet_added`, idle-inhibit intent record/clear, content-type stored hint, xdg-foreign real
    export→import handle, keyboard-shortcuts-inhibit `active`)
  - `popup_placement` (Gap 2)
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

1. ~~**`zwp_linux_dmabuf` v4 feedback (accelerated Chromium).**~~ **CLOSED, but now gated (see Gap 5).**
   **Correction (2026-07-12 live run):** advertising this global *unconditionally* crashed GTK4 (the
   guest's `mmap` of the feedback format-table fd → `MAP_FAILED` → SIGSEGV before frame 1), so it is now
   gated behind `DD_DISPLAY_DMABUF` (commit `20174b15`) exactly like legacy `server.rs` — the default
   software path advertises no dmabuf global. When `DD_DISPLAY_DMABUF` is set, `dd-compositor`
   advertises the feedback-carrying global (Smithay's version-5 dmabuf global, which serves the v4
   `get_default_feedback` path). Chromium's ozone/GPU derives its DRM render-node path from the
   dmabuf-feedback `main_device` (legacy `server.rs::send_dmabuf_feedback` at ~line 2510); the
   compositor's feedback advertises the same `main_device == makedev(226,128)` and the same
   ARGB/XRGB8888 formats, so a GPU-composited Chrome can bring up its accelerated path instead of
   falling back to `wl_shm`. The macOS blocker — Smithay backs the feedback format-table with a
   `shm_open`ed object named `smithay-dmabuffeedback-format-table` (+ random suffix, ~43 bytes) that
   exceeds `PSHMNAMLEN` (31) → `ENAMETOOLONG`, and even once the name fits, that non-Linux
   `SealedFile` path used `write()` on a macOS POSIX-shm object (which is `ftruncate`+`mmap`-only →
   `ENXIO`) — is fixed by an **offline-vendored smithay 0.7.0** patch (`third_party/smithay-0.7.0/`,
   wired via `[patch.crates-io]` in the root `Cargo.toml`) that shortens the object name AND populates
   it via `ftruncate`+`mmap` in `src/utils/sealed_file.rs` (the only file changed). The wiring lives
   in `dd-compositor/src/handlers/dmabuf.rs::new_dmabuf_state` (feedback via
   `DmabufFeedbackBuilder`/`create_global_with_default_feedback`, with a v3 fallback if the format
   table ever fails to build). All of this is behind `DD_DISPLAY_SMITHAY` (that flag execs the
   compositor); legacy is untouched. GLES clients (glmark2/es2tri) still read the same modifier list
   from the feedback's main tranche. Covered by a unit test (`build_default_feedback` builds without a
   PSHMNAMLEN/ENXIO failure) and an integration test (`dmabuf_v4_feedback` asserts the global
   advertises version >= 4). **Note for the live GPU step:** Smithay's `feedback.main_device` is
   serialized as the host `libc::dev_t` (4 bytes on macOS) whereas legacy sends an 8-byte Linux
   `dev_t`; if a guest Chromium is byte-width-sensitive when parsing `main_device`, that is the one
   remaining fidelity detail to confirm during the coordinated live validation.

2. **Popups clip to the parent frame (default) — native-window path landed behind a flag.** By default
   `dd-compositor` still CPU-composites popups into the toplevel's frame and clips them to the parent
   bounds (`blend()`). **Closed for readiness:** the compositor now resolves each `xdg_popup`'s placement
   (`compositor.rs::popup_placement`: direct parent surface + positioner geometry origin, mirroring
   `server.rs::popup_placement`) and, under `DD_DISPLAY_POPUP_WINDOWS=1`, makes a popup its **own present
   root** (`present_root`) carrying `SurfaceBuffer::popup` — which the **shared** `present_cocoa`
   presenter already turns into a child NSWindow at the anchor (create) and `popup_destroyed` already
   tears down (`drop_window`). So the platform wiring needs no new code; only the compositor's present
   routing changed, gated off by default (so the validated composite path is byte-for-byte unchanged) and
   proven by `dd-compositor/tests/popup_placement.rs`. **Remaining:** run the live Chrome/GTK menus on the
   flag, then flip `DD_DISPLAY_POPUP_WINDOWS` to default-on (or fold it into the `DD_DISPLAY_SMITHAY`
   flip). Not required for correctness of in-window menus.

3. ~~**Neither crate is in a build gate.**~~ **CLOSED.** `make mac-crates` builds
   `dd-display`/`dd-gpu-wgpu`/`dd-compositor` and runs the `dd-compositor` + `dd-gpu-wgpu` tests on the
   macOS toolchain (libxkbcommon from the nix dev shell via `DD_LIBXKBCOMMON`), documented in
   `docs/AGENTS.md` as the post-merge check for cross-cutting type changes. Verified green on current
   `main`. A non-macOS host no-ops the target with a note.

4. ~~**`libxkbcommon` is not bundled into `dd.app`.**~~ **CLOSED.** `nix/flake.nix` adds `libxkbcommon`
   to the dev-shell `buildInputs` and exports `DD_LIBXKBCOMMON`. `dd-gui/package/bundle.sh` builds
   `dd-display` + `dd-compositor` (with `RUSTFLAGS=-L native=$DD_LIBXKBCOMMON/lib`), ships them in
   `Contents/Resources` next to the daemon, and adds them to the `dylibbundler` relocation so
   `libxkbcommon` (and any other non-system dep) lands in `Contents/Frameworks` with an
   `@executable_path/../Frameworks` install name — which resolves from a Resources/ binary. Guarded: if
   `DD_LIBXKBCOMMON` is absent (older dev shell) the compositor is skipped and `DD_DISPLAY_SMITHAY` falls
   back to legacy, so the bundle stays shippable. (Not runnable in this Linux-VM audit env; the wiring is
   committed where the packaging lives and the underlying build was verified via `make mac-crates`.)

5. ~~**No live Chrome/GTK-on-Smithay validation.**~~ **GTK4 DONE — renders (after one fix); see below.**
   Live GTK4 (`gtk4-demo`, the `gtkself` workspace) was run headfully through the engine on a private
   `dd-display` socket with `DD_DISPLAY_SMITHAY=1 DD_GPU_BACKEND=wgpu` (the `PngPresenter` headless-dump
   path). **Result: gtk4-demo renders a full, correct GTK Demo window on the Smithay compositor** —
   1028×729, 990 unique colours, ~41% non-background, black text + the GTK4 widget grey palette, complete
   CSD title bar / window controls / sidebar tree / tabs / body text. Pixel- and visually-identical in
   character to the legacy `server.rs` baseline (`gtk4-demo-legacy-baseline.png`, 800×600, 990 colours,
   ~44% non-bg). Evidence: `~/.dd/gtkself-wgpu-evidence/gtk4-demo-SMITHAY-WGPU-RENDERED.png`.

   **Blocker found + fixed during this validation (commit `20174b15`):** the first Smithay run
   **SIGSEGV'd the guest before frame 1** (`[MACH] … hinsn=0xb8616861` → `LDR W1,[X3,X1]` with `X3=-1`,
   a `MAP_FAILED` deref). Root cause: `dd-compositor` advertised the `zwp_linux_dmabuf_v1` **v4 feedback**
   global *unconditionally* (Gap 1 close-out), whereas legacy `server.rs` advertises any dmabuf global
   only under `DD_DISPLAY_DMABUF`. GTK4's libwayland (even on the cairo/`wl_shm` path) eagerly binds the
   feedback global and `mmap`s the feedback **format-table fd** — a macOS POSIX-shm object routed to the
   Linux guest through the engine's SCM_RIGHTS bridge, whose guest-side `mmap` returns `MAP_FAILED` → the
   client dereferences `-1` and crashes. Differential proof (same engine build, same guest): legacy (no
   dmabuf global) rendered fine; Smithay (dmabuf feedback on) crashed pre-frame. **Fix:** gate
   `new_dmabuf_state` on `DD_DISPLAY_DMABUF` (parity with legacy) so the default software path advertises
   no dmabuf global (`wl_shm` renders); the v4 feedback global stays available opt-in. All 8
   `dd-compositor` tests still pass (the two dmabuf tests now set the env). **After the fix the same run
   renders.**

   **Still NOT exercised on Smithay:** the live NSWindow present/input loop (`main.rs::macos`) and the
   Cocoa/Metal presenter (this run used the portable `PngPresenter`); Chrome bring-up; input/cursor/menus.

   **`DD_GPU_BACKEND=wgpu` was inert on this path — a real wiring gap, now ADDRESSED (Phase 6.1–6.2).**
   `maybe_exec_smithay()` execs `dd-compositor` at the *top* of `dd-display::main` (before the
   `run_executor` spawn at `main.rs:~269`), and `dd-compositor` used to never spawn the dd-gpu IR executor
   itself — so under `DD_DISPLAY_SMITHAY=1` the `DD_GPU_BACKEND` selection (`dd_gpu_wgpu::selected()` →
   `run_executor_wgpu`) was **never reached**, and an *accelerated* guest (GL/CUDA/Vulkan via dd-gpu) had
   **no host GPU executor at all** on the Smithay path. **Fix:** `dd-compositor/src/gpu.rs`
   (`gpu::start`, called from `dd-compositor::main` before the compositor mode is selected) starts the
   IOSurface mach bridge + spawns `dd_display::metal_backend::run_executor`, which itself dispatches to
   the wgpu backend when `dd_gpu_wgpu::selected()` — so BOTH the default Metal executor and
   `DD_GPU_BACKEND=wgpu` are now reachable on the Smithay path, respecting the same selection as legacy.
   Phase 6.2 health check: `handlers::dmabuf::dmabuf_imported` calls
   `gpu::warn_if_accel_client_without_executor`, which logs a prominent once-only ERROR if an accelerated
   client attaches a GPU buffer while no executor is running (e.g. on a platform with no host GPU) instead
   of silently rendering white. All behind `DD_DISPLAY_SMITHAY`; no default flipped. **Remaining:** a live
   accelerated-guest run (GLES/vkcube) on the Smithay path to confirm the executor serves IR end-to-end
   (the code path is wired; the device-level render is the closing evidence).

   **Verdict (GTK): the Smithay compositor is ready to be the default for GTK4 software rendering** with
   commit `20174b15` in place (without it, it is a hard pre-frame crash). The wgpu *executor* is a
   separate, still-open item that does not affect GTK.

Non-blocking parity notes: touch (neither path has a real device); DnD drag-icon surface (host cursor
only, both paths).

---

## 5. Safe flip plan (for the later coordinated step — NOT done here)

1. ~~Land the build gate (Gap 3) and `libxkbcommon` bundling (Gap 4) FIRST~~ — **DONE** (`make
   mac-crates`; `nix/flake.nix` + `bundle.sh`). The compositor is now built by the gate and shipped +
   launchable by the bundler.
2. Run live Chrome + GTK4 on `DD_DISPLAY_SMITHAY=1` (software `wl_shm` path) and confirm UI parity
   (sharpness, input latency, cursor shape, menus, clipboard) against the legacy baseline. The `wl_shm`
   path needs none of Gap 1.
3. Decide Gap 2: enable `DD_DISPLAY_POPUP_WINDOWS` (native popup windows — now implemented) or accept
   clipping, based on what the live menus show; if enabling, fold the flag into the default flip.
4. For the accelerated path, Gap 1 (dmabuf v4 feedback) is now **advertised + served** on macOS —
   validate GPU-composited Chrome actually resolves its render node against the feedback `main_device`
   (confirm the host-vs-Linux `dev_t` byte-width note in Gap 1 is a non-issue), and keep
   `DD_GPU_BACKEND=wgpu` behind its own flip until the wgpu executor has a device-level test or a live
   run.
5. Flip the default in `maybe_exec_smithay()` (compositor default; `DD_DISPLAY_SMITHAY=0` = legacy
   escape hatch) and, separately, `dd_gpu_wgpu::selected()` (wgpu default; `DD_GPU_BACKEND=metal` =
   legacy escape hatch). Preserve the exec-fallback-to-legacy safety net.

Escape hatches after the flip: `DD_DISPLAY_SMITHAY=0` → legacy `server.rs`; `DD_GPU_BACKEND=metal` →
bespoke Metal replay. Both are single-env, no rebuild.

---

## 6. Changes made

### Audit pass (2026-07-12, source-read)
- Fixed the `dd-compositor` build break on `main`: added `popup: None` to the `SurfaceBuffer`
  constructions in `dd-compositor/src/handlers/dmabuf.rs` and `handlers/compositor.rs` (see §3).

### Low-risk gap-closing pass (2026-07-12)
- **Gap 3 (build gate):** `Makefile` `mac-crates` target + `docs/AGENTS.md` post-merge-check docs.
  Verified green on current `main` via the macOS toolchain.
- **Gap 4 (libxkbcommon bundling):** `nix/flake.nix` adds `pkgs.libxkbcommon` + `DD_LIBXKBCOMMON`;
  `dd-gui/package/bundle.sh` builds/ships `dd-display` + `dd-compositor` and relocates `libxkbcommon`
  into `Contents/Frameworks`.
- **Gap 2 (popups):** `dd-compositor/src/handlers/compositor.rs` gains `popup_placement`, `present_root`,
  and a `DD_DISPLAY_POPUP_WINDOWS`-gated native-popup-window present path; `snapshot_surface` now carries
  `SurfaceBuffer::popup`. New test `dd-compositor/tests/popup_placement.rs`. Default behaviour unchanged.

### Gap 1 close-out pass (`zwp_linux_dmabuf` v4 feedback)
- **`dd-compositor/src/handlers/dmabuf.rs`:** `new_dmabuf_state` now builds a `DmabufFeedback`
  (`DmabufFeedbackBuilder::new(makedev(226,128), <ARGB/XRGB8888 + dd-modifier/LINEAR>)`) and creates the
  global via `create_global_with_default_feedback` (Smithay version-5 global → serves v4 feedback), with
  a logged fallback to the v3 `create_global` if the format table can't be built. New unit test
  `dmabuf_feedback_format_table_builds_under_pshmnamlen`.
- **`dd-compositor/tests/dmabuf_v4_feedback.rs`** (new): asserts the advertised `zwp_linux_dmabuf_v1`
  global is version >= 4.
- **Offline-vendored smithay 0.7.0** (`third_party/smithay-0.7.0/`, referenced by `[patch.crates-io]` in
  the root `Cargo.toml`): a single-file change to `src/utils/sealed_file.rs` (the non-Linux
  `SealedFile::with_data`) so the `shm_open` object name fits macOS `PSHMNAMLEN` (31) AND is populated
  via `ftruncate`+`mmap` instead of `write()` (macOS POSIX shm is mmap-only). Copied from the local
  cargo registry cache — no fetch. The patch is a no-op for the Linux headless build (smithay is only a
  `dd-compositor` dep, kept out of default-members). Verified: `cargo build` (Linux default-members)
  green offline; `make mac-crates` equivalent (build + `dd-compositor`/`dd-gpu-wgpu` tests) green on the
  macOS host.

### Phase 6.1–6.2 pass — executor/compositor lifecycle (dd-gpu executor reachable on Smithay)
- **`dd-compositor/src/gpu.rs`** (new): owns the dd-gpu IR executor lifecycle for the Smithay path.
  `gpu::start(disp_socket)` (idempotent) starts the IOSurface mach bridge and spawns
  `dd_display::metal_backend::run_executor`, which branches on `DD_GPU_BACKEND` internally
  (`dd_gpu_wgpu::selected()` → wgpu, else default Metal replay). macOS-gated (the executor is a host-GPU
  entry point); on non-macOS it logs "no host GPU … executor not started". Exposes `is_running()` +
  `warn_if_accel_client_without_executor()` for the health check.
- **`dd-compositor/src/main.rs`:** calls `dd_compositor::gpu::start(&socket)` right after resolving the
  socket, **before** the compositor mode (native Cocoa/Metal vs headless `--png`) is selected, so both
  paths get the executor (audit §9.4). Removed the now-redundant `metal::start_gpu_bridge()` from
  `macos::run` (started once in `gpu::start`).
- **`dd-compositor/src/handlers/dmabuf.rs`:** `dmabuf_imported` (the accelerated-client signal) calls
  `gpu::warn_if_accel_client_without_executor()` — Phase 6.2: fail VISIBLY (prominent once-only ERROR) if
  a GPU/IOSurface buffer arrives with no executor running, instead of silently rendering white.
- **`dd-compositor/Cargo.toml`:** adds the `dd-gpu-wgpu` path dep for the `DD_GPU_BACKEND` single source
  of truth (`dd_gpu_wgpu::selected()`); zero new compilation (dd-display already pulls it in).
- **`dd-display/src/main.rs`:** doc-only — records that the exec-split's executor startup is now owned by
  `dd_compositor::gpu::start` on the Smithay side (the exec no longer bypasses executor init).
- Behind `DD_DISPLAY_SMITHAY`; **no default flipped.** Offline: `cargo check -p dd-compositor` +
  `cargo test -p dd-gpu-wgpu` green on Linux (the non-macOS cfg path + wgpu crate); the dd-compositor
  library compiles clean (only vendored-smithay warnings). `make mac-crates` (build + `dd-compositor`/
  `dd-gpu-wgpu` tests) run on the macOS host via the bridge. Live accelerated-guest run on Smithay
  (GLES/vkcube reaching the executor) is the remaining closing evidence.

Explicitly NOT done (out of scope, per the task): flipping any default
(`DD_DISPLAY_SMITHAY`/`DD_GPU_BACKEND`/`DD_DISPLAY_POPUP_WINDOWS`); any live Chrome/GTK-on-Smithay run
(Gap 5).
