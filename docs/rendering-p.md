# Rendering Pipeline Handoff

Status: stopped on 2026-07-09. This is the state of the Chrome GPU/native-window debugging work, including what was fixed, what still fails, where the artifacts are, and the shortest path to resume.

## Goal

Run Google Chrome inside the dd Linux container using the accelerated GPU path, while presenting as a native macOS window. The intended end state is:

- Chrome renders through the GPU/Metal path, not the slow software-only path.
- The window appears on macOS as a normal native surface, ideally frameless/content-only for this debugging path.
- Chrome UI and web content are both visible, sharp, correctly scaled, and clipped to the requested size.
- Mouse and keyboard events enter the visible Chrome window and reach Chrome's Wayland/Ozone client.
- The pipeline works with static content, so network/firewall behavior is not part of the rendering diagnosis.

## Current Verdict

Chrome now gets far enough to create GPU-backed IOSurface content and present it through `dd-display --metal`, but the visible window is still architecturally wrong:

- The visible Metal-presented surface is owned by the GL shim's private Wayland connection.
- Chrome/Ozone's real browser Wayland connection owns the `wl_seat`, XDG window state, and input-capable surface.
- Therefore rendering and input are split across two different Wayland clients.
- The visible shim surface is not input-capable, so clicks go to the visible macOS window but cannot naturally reach the browser's Wayland client.
- Geometry is still mismatched: latest run presented `532x384` for a requested `512x384`, with full UVs and no crop.
- This explains the observed white/black borders, weird padding, content overflowing outside the expected Chrome bounds, tiny/blinking windows in some runs, and non-interactive UI.

The correct long-term fix is to stop the GL shim from creating/presenting its own private Wayland toplevel and instead import/commit the IOSurface onto Chrome/Ozone's original `wl_surface`. That likely requires libwayland proxy capture/interposition in the shim.

## Latest Run

Latest Chrome run:

```sh
/Users/x/dd/dd/target-chrome-codex/chromium-run-gpu_retry_215402
```

Latest visual GPU regression run:

```sh
/Users/x/dd/dd/target-chrome-codex/visual-gpu-runs/run_20260709_213803
```

No Chrome/display processes were still active when this handoff was written.

The latest Chrome run used the static data-URL/local-content path, not an external website. A firewall blocking network content does not explain the broken Chrome UI. The browser process was running and `dd-display` did receive an IOSurface.

Important log lines from `target-chrome-codex/chromium-run-gpu_retry_215402/dd-display.log`:

```text
dd-display[window-metal]: client connected (fd 5, 1 live)
dd-display: GPU bridge cached IOSurface id=97
dd-display[window-metal]: client connected (fd 11, 2 live)
dd-display[metal][present-debug]: event=create frame=1 sid=6 surf=532x384 texture=532x384 uv=[0.000000,0.000000,1.000000,1.000000] iosurface=97 content_bounds=(x=0 y=0 w=532 h=384) layer_drawable=532x384 drawable_tex=none clear=white rgba=(1,1,1,1)
dd-display[metal][present-debug]: event=present frame=1 sid=6 surf=532x384 texture=532x384 uv=[0.000000,0.000000,1.000000,1.000000] iosurface=97 content_bounds=(x=0 y=0 w=532 h=384) layer_drawable=532x384 drawable_tex=532x384 clear=white rgba=(1,1,1,1)
```

Important log line from `launch.log`:

```text
gl_shim: surface_up 532x384
```

Expected size for the current bounded Chrome run was `512x384`. The latest present is therefore still full backing size, not clipped/cropped/mirrored to the intended Chrome logical size.

## What Was Fixed Or Added

### dd-display geometry and diagnostics

Files:

- `dd-display/src/server.rs`
- `dd-display/src/present_cocoa.rs`
- `dd-display/src/lib.rs`
- `target-chrome-codex/run_chrome_gpu_bounded.sh`

Implemented:

- Parsed and stored `xdg_surface.set_window_geometry`.
- Added pending/committed XDG geometry state.
- Updated surface mapping so viewport, buffer scale, and XDG window geometry can affect source/destination mapping.
- Added an external logical crop path for cross-client geometry mirroring.
- Added `Server::focused_logical_geometry()`.
- Added `Server::set_external_logical_crop()`.
- Added `DD_DISPLAY_MIRROR_INPUT_GEOMETRY=1` experiment in `present_cocoa.rs`.
- Added input diagnostics under `DD_DISPLAY_INPUT_DEBUG=1`.
- Added Metal present diagnostics under `DD_DISPLAY_PRESENT_DEBUG=changes` or `all`.
- Forwarded `DD_DISPLAY_INPUT_DEBUG`, `DD_DISPLAY_PRESENT_DEBUG`, and `DD_DISPLAY_MIRROR_INPUT_GEOMETRY` through `target-chrome-codex/run_chrome_gpu_bounded.sh`.

Validation that passed after these changes:

```sh
cargo test -p dd-display external_logical_crop_maps_presenting_surface -- --nocapture
cargo test -p dd-display shm_client_draws_a_frame_the_server_composites -- --nocapture
```

The release `dd-display` binary was verified as a native macOS binary:

```text
target/release/dd-display: Mach-O 64-bit arm64 executable
```

### GL shim rendering fixes and probes

File:

- `dd-tests/guests/gl_shim.c`

Implemented or partially implemented:

- Retained-default-surface tracking:
  - `g_default_surface_valid`
  - `g_default_full_clear_since_swap`
- Default-FBO passes can load prior surface contents when appropriate.
- Clear-only default-FBO swaps now record a full-target clear call instead of emitting an empty submit.
- Single-channel uploads were corrected:
  - `GL_RED -> (r, 0, 0, 1)`
  - `GL_ALPHA -> (0, 0, 0, a)`
  - `GL_LUMINANCE -> (l, l, l, 1)`
- Removed a bad vertical row flip on single-channel uploads.
- Added a shim-owned XDG geometry fallback that can send `xdg_surface.set_window_geometry`.

Important caveat: the shim-owned geometry fallback should not be deployed as-is. A reviewer found that `wl_send_window_geometry()` suppresses sending geometry when it matches backing size. XDG window geometry is persistent, so after a prior cropped/non-default geometry, returning to full backing needs an explicit clearing `set_window_geometry`. Fix this stale-geometry edge case before relying on the shim fallback.

### GUI/GPU test coverage

Files and directories:

- `dd-tests/guests/gui_matrix/`
- `target-chrome-codex/tests/visual_gpu_regression.py`
- `target-chrome-codex/tests/chrome_dump_bounds_validator.py`
- `target-chrome-codex/tests/README.md`

Added or used probes include:

- `retained_frame_partial_load.c`
- `single_channel_texture_probe.c`
- `gui_egl_clear_only_swap.c`
- `gui_egl_r8_alpha_orientation.c`
- `gui_egl_fifth_sampler.c`
- `gui_egl_draw_count_sentinel.c`
- `gui_egl_renderbuffer_msaa_resolve.c`
- existing GUI matrix probes for textured quads, VAO state, texture formats, resize lifecycle, content composite, damage/scissor, framebuffer blit, premul blend, etc.

Added `chrome_dump_bounds_validator.py`, a stdlib PNG validator for detecting wrong Chrome dump dimensions and right-edge spill/fringe. It already caught the bad 532-wide dump against the expected 512-wide logical target.

Known validation issue:

- `python3 target-chrome-codex/tests/visual_gpu_regression.py` failed in the last pass because guest binaries could not load `libc.so`, not because it found a rendered-frame mismatch. Fix the guest runtime/library packaging for those probes before treating visual regression output as authoritative.

## What Is Still Broken

### 1. Rendering and input are on different Wayland clients

This is the main architectural bug.

Observed model:

- Browser/Ozone connection:
  - owns the real `wl_seat`
  - owns pointer/keyboard focus protocol
  - owns the real XDG window semantics
  - is the client that should receive events
- GL shim connection:
  - owns the currently visible IOSurface-presenting surface
  - creates `sid=6`
  - presents the Metal content
  - is not input-capable

Result:

- The user sees the shim window.
- The browser client is the one that needs input.
- `dd-display` routes native macOS events by visible NSWindow/surface ownership, so events hit the wrong client or are dropped.

Short-term workaround attempted:

- `DD_DISPLAY_MIRROR_INPUT_GEOMETRY=1` tries to mirror browser geometry onto the shim-presented surface.

Current result:

- The latest run did not show `dd-display[mirror-geometry]` logs before the presented frames.
- The presented frames stayed `532x384` full UV.
- This may be a timing issue: the shim presents before the browser focused geometry is known, then stops presenting before the mirror has a chance to correct a later frame.
- It may also be the wrong source geometry: browser XDG geometry can describe content bounds rather than the whole IOSurface backing.

### 2. Geometry/crop/scale is still wrong

Latest hard evidence:

- Requested Chrome window size: `512x384`.
- Shim `surface_up`: `532x384`.
- `dd-display` Metal present: `surf=532x384 texture=532x384 uv=[0,0,1,1] content_bounds=(x=0 y=0 w=532 h=384)`.

User-visible symptoms this matches:

- white background behind Chrome
- weird border/padding around content
- content extending beyond the apparent Chrome window
- tiny window or blinking when scale/size path changes
- blurry output when backing/logical scale are mixed

Most likely causes:

- The shim is presenting backing dimensions instead of Chrome's requested logical dimensions.
- The `xdg_surface.set_window_geometry` crop is either missing, late, stale, or attached to the wrong client.
- The visible NSWindow/CAMetalLayer size follows shim backing instead of browser logical bounds.
- Layer clear is white (`clear=white` in present-debug), so uncovered or incorrectly cropped regions appear as white background.

### 3. Events still do not propagate

The input routing instrumentation exists, but the architectural split remains. Until the visible IOSurface is associated with Chrome's real browser surface, event forwarding will be a workaround.

Immediate debug env:

```sh
DD_DISPLAY_INPUT_DEBUG=1
```

Look for:

- target NSWindow
- owner client/surface
- `can_receive_input`
- forwarding candidates
- selected/dropped reason

Expected failure pattern:

- visible surface owner is the shim client
- `can_receive_input=false`
- browser client is a candidate but not the owner of the visible surface

### 4. The window is not yet the desired frameless/content-only presentation

The current path still uses an AppKit `NSWindow` hosting a `CAMetalLayer`. For debugging Chrome, the requested behavior is effectively content-only/frameless. That has not been completed. Do not spend time polishing frame style before fixing surface ownership, geometry, and event routing.

### 5. Chrome can animate briefly, then stall

The early tab loading spinner/animation was observed in several runs. That proves at least some command stream/present path can execute. Later complete stalls can come from:

- the shim stops producing new present frames after initial frames
- Chrome's frame pipeline stops submitting because the browser/shim/client split leaves it in a confused surface state
- the compositor has no valid mapped target after geometry or event-state divergence

Earlier engine-level wakeup, futex, SEQPACKET, Mojo, and procfs blockers were heavily investigated in `docs/ENGINE_HOLES.md`. Many of those lower-level holes are fixed. The current problem is now in the rendering/windowing integration, not the old "Chrome cannot bootstrap child processes" wall.

## What Has Been Ruled Out

- Firewall/network content is not the cause of the broken Chrome UI. Static/local content was used.
- Chrome is not merely failing to launch. It gets far enough for `gl_shim: surface_up 532x384` and `dd-display` to cache/present IOSurface id `97`.
- The latest visible failure is not "no GPU object at all"; there is an IOSurface and Metal present path.
- The current bad border/overflow is not subtle shader correctness alone; present-debug shows a concrete geometry mismatch.
- The old Mojo/zygote/SEQPACKET bootstrap blockers are not the current top issue. They were earlier Chrome bring-up blockers and are documented separately in `docs/ENGINE_HOLES.md`.

## Best Next Steps

### Critical path A: stop shim-owned visible surfaces

This is the real fix.

Implement libwayland interposition/capture in the GL shim so that when Chrome/Ozone creates the actual browser `wl_surface`, the shim can:

- identify the original Chrome/Ozone surface;
- avoid creating a second private visible toplevel for the IOSurface;
- attach/import/commit the IOSurface presentation to Chrome's original surface;
- keep the browser client's `wl_seat`, XDG state, frame callbacks, and surface identity together.

Expected result:

- the visible surface owner is the browser/Ozone client;
- event routing becomes natural instead of cross-client forwarding;
- XDG geometry belongs to the same object being presented;
- crop/scale/title/focus all have one source of truth.

This is more work than the mirror workaround, but it removes the main cause instead of stacking special cases.

### Critical path B: make geometry deterministic while A is in progress

Fix or finish these in order:

1. Fix the shim `set_window_geometry` stale-clearing bug.
2. Make `DD_DISPLAY_MIRROR_INPUT_GEOMETRY=1` log why it did not apply in the latest run:
   - env not enabled;
   - no input-capable source;
   - no focused geometry;
   - multiple candidates;
   - source geometry equal/invalid;
   - target pumped before source geometry arrived.
3. Persist the last known browser geometry and apply it before every shim present, not only during a narrow pump window.
4. Decide whether the source crop should be:
   - browser XDG window geometry;
   - Chrome requested `--window-size`;
   - `wl_egl_window` logical size;
   - content bounds from IOSurface/viewport;
   - or a new explicit shim-to-display hint.
5. Re-run `chrome_dump_bounds_validator.py` after every geometry change.

Useful validator command:

```sh
python3 target-chrome-codex/tests/chrome_dump_bounds_validator.py \
  target-chrome-codex/live-dumps/current-mirror-run.png \
  --expected-width 512 \
  --expected-height 384 \
  --allow-backing-width 532 \
  --content-width 512
```

### Critical path C: input forwarding only as temporary glue

If the architecture cannot be corrected immediately, add a tightly scoped forwarding path:

- visible shim surface receives macOS input;
- `dd-display` forwards pointer/keyboard events to the one browser client with input capability;
- coordinate transform uses the same crop/scale mapping as presentation;
- logs every forwarded event while `DD_DISPLAY_INPUT_DEBUG=1`.

This is a workaround. It should be removed or bypassed when the IOSurface is committed to Chrome's real surface.

### Test/probe path

Before running Chrome repeatedly, keep the smaller accelerated GUI probes green:

```sh
cargo test -p dd-display external_logical_crop_maps_presenting_surface -- --nocapture
cargo test -p dd-display shm_client_draws_a_frame_the_server_composites -- --nocapture
python3 target-chrome-codex/tests/visual_gpu_regression.py
```

Fix the `libc.so` guest-binary issue in `visual_gpu_regression.py` runs first, otherwise the regression suite cannot reliably distinguish harness failure from rendering failure.

Then run Chrome bounded, with static content and verbose display diagnostics:

```sh
CHROME_NATIVE_WINDOW=1 \
CHROME_WINDOW_SIZE=512,384 \
DD_DISPLAY_MIRROR_INPUT_GEOMETRY=1 \
DD_DISPLAY_PRESENT_DEBUG=changes \
DD_DISPLAY_INPUT_DEBUG=1 \
target-chrome-codex/run_chrome_gpu_bounded.sh 120
```

If a native window appears, capture with `SIGUSR1` to `dd-display` and validate the PNG before visually inspecting:

```sh
DPID=$(ps -ax -o pid=,command= | awk '/dd-display --socket .*chromium-run-gpu_retry/ && !/awk/ {print $1; exit}')
kill -USR1 "$DPID"
```

## Files To Inspect First On Resume

- `dd-display/src/present_cocoa.rs`
  - `DD_DISPLAY_PRESENT_DEBUG`
  - `DD_DISPLAY_INPUT_DEBUG`
  - `DD_DISPLAY_MIRROR_INPUT_GEOMETRY`
  - `route_input`
  - multi-client pump/mirror timing
- `dd-display/src/server.rs`
  - `xdg_surface.set_window_geometry`
  - `surface_mapping`
  - `focused_logical_geometry`
  - `set_external_logical_crop`
- `dd-tests/guests/gl_shim.c`
  - `surface_up`
  - `wl_send_window_geometry`
  - `wl_commit`
  - `wl_egl_window_create`
  - IOSurface/default-FBO submit path
- `target-chrome-codex/run_chrome_gpu_bounded.sh`
  - env forwarding to native `dd-display`
  - static-content Chrome command line
- `target-chrome-codex/tests/chrome_dump_bounds_validator.py`
  - bounds/crop validation
- `target-chrome-codex/tests/visual_gpu_regression.py`
  - small GUI/GLES regression harness

## Agent Results Collected

Completed worker areas:

- input diagnostics
- present-debug instrumentation
- geometry implementation and review
- Chrome dump validator
- retained-frame/default-FBO behavior
- single-channel texture upload behavior
- GUI rendering probes
- architecture investigation
- clear-only swap behavior
- shim deploy recipe
- shim geometry review
- cross-client geometry mirror

Main combined conclusion from the agents:

- There are many normal rendering holes to keep closing with probes, but the Chrome-specific visible breakage is dominated by the split between the shim-presented IOSurface surface and the browser-owned Wayland/input/XDG surface.
- More shader/probe work is useful, but it will not by itself make Chrome interactive or correctly clipped until surface ownership is fixed.

## Glossary

- Zygote: Chrome's helper parent process used to fork sandboxed renderer/GPU/utility child processes. Earlier work fixed several engine holes needed for zygote/child bootstrap.
- Ozone: Chrome's platform abstraction layer. In this path it talks Wayland.
- XDG geometry: Wayland shell surface logical window geometry. It can define the visible/content rectangle inside a larger backing buffer.
- IOSurface: macOS shareable GPU/CPU surface object. This is the object being handed to Metal for accelerated presentation.
- Shim: the guest GL/EGL/Wayland shim in `dd-tests/guests/gl_shim.c` that lets Linux Chrome/GLES rendering reach the host Metal path.
