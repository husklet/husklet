# GPU, Display, and Sentry Compatibility Gaps

Date: 2026-07-10

These findings were verified in isolated worktrees `/Users/x/dd/dd-verifier6` and `/Users/x/dd/dd-verifier6b`. Main worktree was not modified.

## Untrusted Split Breaks Linux `EFAULT` Compatibility

Priority: P1
Impact: compatibility breakage and silent wrong errno behavior under `DDJIT_UNTRUSTED=1`
Confidence: High

Evidence:

- The worker marshaling path copies guest pointers directly while packaging requests for the sentry: `dd-jit-darwin/src/runtime/os/linux/sentry.c:1472`.

Why this is bad:

The sentry is meant to preserve syscall semantics while moving authority to a helper process. Bad guest pointers should produce Linux-style `EFAULT`. Instead, the worker can fault or marshal wrong data before the sentry can validate the pointer.

Isolated proof:

PoC added in isolated worktree by registering existing `edge_efault.c` as `efault-untrusted`.

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-verifier6/target-agent6 cargo run -p dd-tests -- -e aarch64 efault-untrusted
CARGO_TARGET_DIR=/Users/x/dd/dd-verifier6/target-agent6 cargo run -p dd-tests -- -e x86_64 efault-untrusted
```

Results:

- aarch64: fails with `jit 255/""` vs native `efault ... =1`.
- x86_64: exits `0` but silently wrong; all bad-pointer verdicts are `0` instead of `1`.

## Multiple `wl_surface.frame` Requests Collapse To One

Priority: P1
Impact: unresolved callback objects and stalled frame pacing
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-workerD-render-audit-20260710`.

Evidence:

- `Surface` stores one `pending_frame: Option<u32>`: `dd-display/src/server.rs:201`.
- Each `wl_surface.frame` overwrites the previous callback id: `dd-display/src/server.rs:752`.
- Commit emits only that one callback: `dd-display/src/server.rs:818`.

Why this is bad:

Wayland permits multiple frame callback requests before a commit. Earlier callback objects should be completed or otherwise handled; overwriting them can leave clients waiting forever.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/target-workerD-render-audit cargo test -p dd-display commit_completes_all_pending_frame_callbacks -- --nocapture
```

Result: failed as expected; observed callbacks `[102]`, expected `[101, 102]`.

## Metal Replay Silently No-Ops Supported IR Commands

Priority: P1
Impact: submitted compute/copy/readback work can be skipped while frame success continues
Confidence: Medium-high

Evidence:

- IR defines command variants such as `Dispatch`, `CopyBufferToBuffer`, and `CopyTextureToBuffer`.
- Metal replay falls through to `_ => {}` for unhandled encoder ops: `dd-display/src/metal_backend.rs:2109`.

Why this is bad:

If a guest emits a supported IR command that the Metal backend does not handle, the command stream can appear accepted while the work is missing. That is silent render or readback corruption.

Verification:

Add a Metal replay probe that performs a copy/readback-dependent operation and asserts the destination changed. The backend should return an error or implement the command, not silently skip it.

## Presenter Failures Still Release Buffers And Fire Frame Callbacks

Priority: P2
Impact: clients advance frame pacing after a skipped presentation
Confidence: Medium

Evidence:

- `MetalPresenter::present` returns early when IOSurface lookup fails: `dd-display/src/present_cocoa.rs:557`.
- It can also return early if drawable acquisition fails: `dd-display/src/present_cocoa.rs:616`.
- `Server::commit` does not receive presentation success and still releases the buffer and fires frame callbacks: `dd-display/src/server.rs:813`, `dd-display/src/server.rs:818`.

Why this is bad:

Clients can reuse a buffer and schedule the next frame even though nothing reached the screen. This hides display failures as normal frame completion.

Verification:

Inject an invalid IOSurface id or no-drawable presenter state and assert `wl_buffer.release`/`wl_callback.done` behavior matches the chosen failure policy.

## xdg Configure/Ack Race Allows Pre-Ack Presentation

Priority: P1
Impact: xdg-shell clients can present before acknowledging configure
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-workerJ-display-gpu-20260710`.

Evidence:

- `maybe_configure_surface` sends configure and immediately marks the surface configured: `dd-display/src/server.rs:871`, `dd-display/src/server.rs:875`.
- `ack_configure` is accepted but ignored: `dd-display/src/server.rs:1178`.
- Commit presents attached buffers once configured state is true: `dd-display/src/server.rs:807`.

Why this is bad:

xdg-shell requires clients to acknowledge configure serials before committing configured content. Marking configured before ack can hide client protocol errors and present out-of-order buffers.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-workerJ-display-gpu-target cargo test -p dd-display audit_ -- --nocapture
```

Result: `audit_xdg_buffer_commit_before_ack_is_not_presented` failed as expected.

## Data-Device Objects Are Inert

Priority: P1
Impact: clipboard integrations silently fail
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-workerJ-display-gpu-20260710`.

Evidence:

- `wl_data_device_manager` is advertised as v3: `dd-display/src/server.rs:541`.
- Its child objects are intentionally inert: `dd-display/src/server.rs:1463`.

Why this is bad:

Clients can create data sources/devices and call selection APIs without any error or observable effect. Clipboard and drag-and-drop features appear supported but do not work.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-workerJ-display-gpu-target cargo test -p dd-display audit_ -- --nocapture
```

Result: `audit_data_device_set_selection_is_not_silently_swallowed` failed as expected.

## dmabuf Advertises LINEAR Buffers It Cannot Use

Priority: P2
Impact: clients can choose an advertised buffer path that yields unusable buffers
Confidence: Medium-high

Evidence:

- dmabuf is advertised when `DD_DISPLAY_DMABUF` is set: `dd-display/src/server.rs:547`.
- The server advertises a LINEAR modifier alongside dd-private modifiers: `dd-display/src/server.rs:650`.
- `dmabuf_params` only records dd-private IOSurface-tagged modifiers: `dd-display/src/server.rs:1320`.
- A create call without a dd IOSurface tag produces `Obj::Other`: `dd-display/src/server.rs:1354`.

Why this is bad:

A client choosing the advertised LINEAR modifier can create a buffer object that the compositor cannot present, producing missing frames or fallback confusion.

Verification:

Run a dmabuf client that selects LINEAR and assert the server either supports it or sends a protocol failure instead of an inert object.

## Metal Backend Skips Missing Bind-Group Resources

Priority: P2
Impact: cross-backend divergence and black/wrong frames
Confidence: High

Evidence:

- Metal bind-group creation records referenced ids: `dd-display/src/metal_backend.rs:1739`.
- Replay only binds resources if they are found: `dd-display/src/metal_backend.rs:1885`, `dd-display/src/metal_backend.rs:1907`.
- Software/mock backends validate bind-group resources up front: `dd-gpu/src/software.rs:281`, `dd-gpu/src/mock.rs:148`.

Why this is bad:

Missing buffers/textures/samplers become nil bindings or no-op draws on Metal, while other backends reject them. That can turn invalid IR into black frames instead of an error.

Verification:

Add a backend parity test for bind groups referencing missing resources and require consistent `GpuError` behavior.

## Wayland Destructors Do Not Remove Objects

Priority: P1
Impact: stale compositor state and client object-id reuse failures
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-O`.

Evidence:

- Dispatch routes buffers, outputs, regions, and inert objects to a no-op destroy path: `dd-display/src/server.rs:495`.
- `wl_surface.destroy` falls through the surface default arm, and `xdg_toplevel` only handles `set_title`: `dd-display/src/server.rs:1178`, `dd-display/src/server.rs:1182`.

Why this is bad:

Wayland destructors should remove objects and usually emit `wl_display.delete_id` so clients can reuse ids. Ignoring destroy leaves stale server objects and can break libwayland id reuse.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-O/target-worker-O cargo test -p dd-display wl_surface_destroy_emits_delete_id -- --nocapture
```

Result: failed as intended.

## Destroyed `wl_buffer` Can Still Be Presented

Priority: P1
Impact: stale buffer contents can appear after destroy
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-O`.

Evidence:

- A no-attach commit reuses `current_buffer`: `dd-display/src/server.rs:799`.
- Commit then presents that buffer if extraction succeeds: `dd-display/src/server.rs:807`.
- `wl_buffer.destroy` is ignored by the no-op destroy path: `dd-display/src/server.rs:495`.

Why this is bad:

After a client destroys a buffer, later commits should not present that object. dd can keep presenting stale contents.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-O/target-worker-O cargo test -p dd-display destroyed_buffer_is_not_presented_on_later_commit -- --nocapture
```

Result: failed as intended.

## Metal Render Target Texture Id Can Alias Guest Texture Id `1`

Priority: P2
Impact: guest texture id can silently alias the present target
Confidence: Medium-high

Evidence:

- The executor pre-registers the IOSurface render target as texture id `1`: `dd-display/src/metal_backend.rs:1198`.
- `create_texture` treats an existing texture id without a descriptor as a no-op: `dd-display/src/metal_backend.rs:1485`.

Why this is bad:

If guest IR creates an ordinary texture id `1`, it can silently alias the present target instead of creating a distinct texture, corrupting render output.

Verification:

Submit IR that creates and samples/writes texture id `1` while presenting to an IOSurface and assert it is not aliased with the render target.

## Released Input Objects Remain Active

Priority: P2
Impact: input events can target released pointer/keyboard objects
Confidence: Medium

Evidence:

- `wl_seat.get_pointer/get_keyboard` caches object ids in `self.pointer` / `self.keyboard`: `dd-display/src/server.rs:1367`.
- Released `Obj::Other` objects are ignored by the no-op destroy path: `dd-display/src/server.rs:495`.
- Later pointer/key events still use the cached ids: `dd-display/src/server.rs:1531`, `dd-display/src/server.rs:1583`.

Why this is bad:

Clients that release input objects can still receive events to those ids, violating object lifetime expectations.

Verification:

Create pointer/keyboard objects, release them, then inject input and assert no events target released ids.

## GPU Executor Acks Success After Replay Errors Or Skipped Writes

Priority: P1
Impact: guest believes a frame rendered while stale pixels remain
Confidence: Medium-high

Evidence:

- `replay_stream` errors are logged but the executor still writes ack `1`: `dd-display/src/metal_backend.rs:1201`, `dd-display/src/metal_backend.rs:1209`.
- Out-of-bounds `write_buffer` logs and returns `Ok(())`: `dd-display/src/metal_backend.rs:1465`.
- Out-of-bounds `CopyBufferToTexture` logs and skips the copy without surfacing failure: `dd-display/src/metal_backend.rs:2017`.

Why this is bad:

The guest-side renderer receives a success ack even when the frame did not replay correctly. That converts malformed IR, stale resources, or bounds bugs into silent stale-frame output.

Verification:

Run a guest render that intentionally submits an OOB upload or malformed replay stream, then assert the guest sees failure instead of a rendered-frame ack.

## `DrawIndexed.base_vertex` Is Ignored By Metal Replay

Priority: P1
Impact: indexed draws can use the wrong vertices
Confidence: Medium-high

Evidence:

- IR encodes and decodes `base_vertex`: `dd-gpu/src/ir.rs:773`, `dd-gpu/src/ir.rs:884`.
- Metal replay destructures `DrawIndexed` with `..` and does not use `base_vertex`: `dd-display/src/metal_backend.rs:1976`.

Why this is bad:

APIs such as `glDrawElementsBaseVertex` depend on adding `base_vertex` to fetched vertex indices. Ignoring it renders geometry from the wrong vertex range.

Verification:

Add a small indexed draw probe where `base_vertex` selects visibly different vertices. Expected fix behavior: Metal replay uses the base-vertex draw call or adjusts offsets correctly.

## Rendering Coverage Gaps Are Silent

Priority: P2
Impact: rendering regressions can sit outside default GUI gates
Confidence: High

Evidence:

- Seven GUI probe sources were present but not included in the GUI matrix Makefile/default runner in the isolated worktree:
  - `gui_egl_clear_only_swap`
  - `gui_egl_draw_count_sentinel`
  - `gui_egl_fifth_sampler`
  - `gui_egl_r8_alpha_orientation`
  - `gui_egl_renderbuffer_msaa_resolve`
  - `retained_frame_partial_load`
  - `single_channel_texture_probe`
- Direct GPU replay has one Linux/aarch64 case and it is xfailed: `dd-tests/src/cases/ext/gpu_render_ir.rs:11`.

Proof command:

```sh
comm -23 \
  <(find dd-tests/guests/gui_matrix -maxdepth 1 -name '*.c' -printf '%f\n' | sed 's/\.c$//' | sort) \
  <(sed -n '1,80p' dd-tests/guests/gui_matrix/Makefile | tr ' \t' '\n' | sed 's/\\$//' | rg '^gui_|^chrome_|^single_|^retained_' | sort -u)
```

Suggested gate:

Add a static test that every `dd-tests/guests/gui_matrix/*.c` probe is either in the matrix or listed in a documented exclusion table with owner and reason.

## Shm Pool Mappings Survive Client Disconnect

Priority: P1
Impact: compositor memory leak under client churn
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-T-display-gpu-20260710`.

Evidence:

- `wl_shm.create_pool` maps fd-backed memory and stores `Obj::ShmPool`: `dd-display/src/server.rs:686`.
- There is no disconnect cleanup path that walks pool mappings and releases them.

Why this is bad:

A client can create shm pools and disconnect without sending destroy requests. The compositor should release per-client objects and mappings when the connection dies; otherwise repeated clients leak address space and file-backed mappings.

Isolated proof:

```sh
cargo test -p dd-display poc_disconnect_drops_shm_pool_mappings -- --nocapture
```

Result: failed; shm pool mappings survived connection teardown.

## Focus Transfer Sends Enter Without Leave

Priority: P1
Impact: Wayland clients can keep stale keyboard/pointer focus
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-T-display-gpu-20260710`.

Evidence:

- Focus assignment resets entered flags: `dd-display/src/server.rs:1153`.
- Input helpers send enter/key/motion paths but have no corresponding leave emission for old focus: `dd-display/src/server.rs:1497`.

Why this is bad:

When focus moves between surfaces, the old surface should receive leave events before the new surface receives enter. Missing leave can make clients believe they still own keyboard or pointer focus.

Isolated proof:

```sh
cargo test -p dd-display poc_focus_switch_emits_pointer_and_keyboard_leave -- --nocapture
```

Result: failed; new focus received enter without old focus leave.

## Native Window Close Is Not Propagated

Priority: P2
Impact: window manager close requests do not reach xdg clients
Confidence: Medium-high

Evidence:

- AppKit event routing handles native events in `dd-display/src/present_cocoa.rs:1269`.
- The injection path covers input events: `dd-display/src/present_cocoa.rs:976`.
- `xdg_toplevel` handling currently covers only title-like requests: `dd-display/src/server.rs:1182`.

Why this is bad:

Clicking the native close button should send `xdg_toplevel.close` so the client can exit or prompt. If the close never reaches the Wayland client, windows can become impossible to close cleanly from the host UI.

## Keyboard Repeat Is Internally Contradictory

Priority: P2
Impact: clients receive repeat metadata even though keymap says no repeat
Confidence: Medium

Evidence:

- The compositor sends `repeat_info`: `dd-display/src/server.rs:1383`.
- The generated keymap sets `interpret.repeat = False`: `dd-display/src/keymap.rs:105`.

Why this is bad:

Toolkits combine keymap and protocol repeat information. Contradictory repeat state can produce missing repeats or double-repeat behavior depending on the client.

## Metal Duplicate IDs And Format Fallbacks Diverge From Checked Backends

Priority: P2
Impact: stale resources or wrong texture formats can be accepted silently
Confidence: Medium-high

Evidence:

- Metal resource maps insert texture IDs directly, overwriting duplicates: `dd-display/src/metal_backend.rs:1448`.
- Texture format handling falls back to BGRA for unsupported or unexpected formats: `dd-display/src/metal_backend.rs:195`.

Why this is bad:

Other backends tend to reject invalid or duplicate resource definitions. Silent overwrite and fallback can hide guest bugs and produce frames that use stale resources or different channel formats than requested.

## Pointer Release And Id Reuse Corrupt Input Routing

Priority: P1
Impact: protocol stream corruption after toolkit object churn
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-slot-z`.

Evidence:

- The server stores `wl_pointer` ids for later input routing: `dd-display/src/server.rs:1367`.
- Release/destroy requests on inert objects are ignored: `dd-display/src/server.rs:495`.
- Later input still emits to the cached id: `dd-display/src/server.rs:1497`.

Why this is bad:

Wayland object ids can be released and reused. Sending pointer events to a released/reused id can corrupt the client's protocol stream or deliver input to the wrong object.

Isolated proof:

```sh
cargo test -p dd-display pointer_release_stops_events_to_reused_id -- --ignored --nocapture
```

Result: failed; server emitted `[(6,0), (6,2), (6,5)]` to a released/reused id.

## `xdg_popup` Never Gets Configured Or Mapped

Priority: P1
Impact: menus, tooltips, and context popups can hang invisible
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-slot-z`.

Evidence:

- `get_popup` creates `Obj::Other`: `dd-display/src/server.rs:1158`.
- Initial xdg commits only configure when `find_toplevel` succeeds: `dd-display/src/server.rs:855`.

Why this is bad:

xdg popup surfaces require configure/ack sequencing before clients can map them. Treating popups as inert objects means common toolkit UI such as menus and tooltips can wait forever for a configure event.

Isolated proof:

```sh
cargo test -p dd-display xdg_popup_gets_configure_handshake -- --ignored --nocapture
```

Result: failed; no popup configure messages were emitted.

## Metal Shader Id Can Retain Stale MSL

Priority: P2
Impact: pipelines can keep using an old shader after id reuse
Confidence: Medium-high

Evidence:

- Shader creation does nothing when `msl_from_words` returns `None`: `dd-display/src/metal_backend.rs:1546`.

Why this is bad:

If a shader id previously held MSL and is recreated with non-MSL or empty payload data, the old library may remain in the map. Later pipelines using that id can render with stale shader code rather than failing or replacing the resource.

## Unsupported `wl_shm` Formats Are Accepted

Priority: P2
Impact: arbitrary shm formats can be reinterpreted as pixels
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-aw-gpu-20260710`.

Evidence:

- `wl_shm_pool.create_buffer` stores arbitrary client format: `dd-display/src/server.rs:711`.
- Extraction/presentation proceeds without format validation: `dd-display/src/server.rs:938`.
- PNG conversion only special-cases XRGB: `dd-display/src/present.rs:44`.

Why this is bad:

The compositor should reject formats it did not advertise or cannot decode. Accepting `0xdead_beef` can reinterpret bytes as BGRA/ARGB and present corrupted frames.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-aw-gpu-20260710-target cargo test -p dd-display "server::tests::" -- --nocapture
```

Result: failed; unsupported format `0xdead_beef` still presented one frame.

## Invalid Shm Buffer Offset Can Panic Compositor

Priority: P1
Impact: malformed Wayland client can crash the compositor process
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-bd2-worktree`.

Evidence:

- `wl_shm_pool.create_buffer` stores negative offsets unchecked: `dd-display/src/server.rs:712`.
- Buffer extraction casts offset to `usize` and adds row size before rejecting negative offsets: `dd-display/src/server.rs:956`.

Why this is bad:

Invalid shm buffer offsets should be rejected cleanly with a protocol error or disconnect. dd can panic on overflow during extraction, taking down the compositor.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-bd2-target cargo test -p dd-display shm_buffer_ -- --nocapture
```

Result: `shm_buffer_negative_offset_is_rejected_without_panic` panicked at `server.rs:956` with `attempt to add with overflow`.

## Shm Buffer Stride Smaller Than Row Is Accepted

Priority: P1
Impact: invalid shm rows can overlap and present corrupted pixels
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-bd2-worktree`.

Evidence:

- Validation only checks that stride is positive: `dd-display/src/server.rs:953`.
- Extraction copies `width * 4` bytes from each row: `dd-display/src/server.rs:965`.

Why this is bad:

Wayland shm buffers need enough stride for a full row. Accepting `stride < width * bytes_per_pixel` makes rows overlap and can present corrupted pixels from invalid buffers.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-bd2-target cargo test -p dd-display shm_buffer_ -- --nocapture
```

Result: `shm_buffer_stride_smaller_than_row_is_rejected` failed because the invalid buffer still produced a presentable frame.

## Viewport Source Outside Buffer Is Clamped

Priority: P1
Impact: invalid viewport rectangles silently present different content
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-bg2-display-gpu-20260710`.

Evidence:

- Viewport source handling clips/crops during extraction: `dd-display/src/server.rs:1007`.
- Viewport state is applied later to the presented buffer: `dd-display/src/server.rs:1443`.

Why this is bad:

`wp_viewport.set_source` rectangles outside the buffer should be rejected with a protocol error. Clamping an invalid source such as `x=3, w=4` on a `4x1` buffer silently changes the requested content to a `1x1` crop.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-bg2-display-gpu-20260710-target cargo test -p dd-display viewport_source_outside_buffer_is_rejected_not_clamped -- --nocapture
```

Result: failed because the out-of-bounds source was clamped instead of rejected.

## `wl_surface.set_buffer_scale(0)` Is Silently Normalized

Priority: P2
Impact: invalid Wayland scale mutates state instead of protocol error
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-bl2`.

Evidence:

- `set_buffer_scale` uses `r.i32().max(1)`: `dd-display/src/server.rs:762`.
- The normalized value is stored as surface scale: `dd-display/src/server.rs:764`.

Why this is bad:

Wayland treats scale `0` as invalid. dd silently turns it into scale `1`, changing previously valid state and hiding client protocol bugs.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-bl2-target cargo test -p dd-display wl_surface_set_buffer_scale_zero_is_not_silently_normalized -- --nocapture
```

Result: after valid scale `2`, sending scale `0` changed state to `1`.

## Invalid Viewport Destination Keeps Stale State

Priority: P1
Impact: invalid Wayland viewport state silently reuses old geometry
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-BO2-20260710`.

Evidence:

- Viewport destination state is applied during presentation: `dd-display/src/server.rs:1448`.
- Invalid destination handling does not clear or error the prior valid state: `dd-display/src/server.rs:1003`.

Why this is bad:

`wp_viewport.set_destination(0, 1)` is invalid and should produce a protocol error or disconnect. Ignoring it while keeping an older `2x2` destination makes later commits use stale geometry.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-BO2-target cargo test -p dd-display viewport_invalid_destination_does_not_keep_stale_destination -- --nocapture
```

Result: after a valid `2x2` destination, invalid `set_destination(0, 1)` was ignored and the stale `2x2` mapping remained.

## Metal Sampler Creation Drops Descriptor Fields

Priority: P2
Impact: sampler `mip_filter` and `address_w` silently diverge
Confidence: High

Verification status: Source-proven in isolated worktree `/Users/x/dd/dd-gpu-tex-audit`.

Evidence:

- `SamplerDesc` carries and wire-encodes `mip_filter` and `address_w`: `dd-gpu/src/ir.rs:145`, `dd-gpu/src/ir.rs:491`.
- Metal sampler creation applies min/mag filters and U/V address modes, but not mip filter or W address mode: `dd-display/src/metal_backend.rs:1518`.

Why this is bad:

Sampler descriptors should either apply all supported fields or reject unsupported settings. Dropping fields silently changes texture sampling behavior.

## Metal Cleanup Diverges From Checked Backends

Priority: P2
Impact: Metal resource cleanup coverage does not match backend expectations
Confidence: Medium-high

Verification status: Source-proven in isolated worktree `/Users/x/dd/dd-audit-gpu-cleanup-20260710`.

Evidence:

- Metal backend cleanup paths are spread through resource-specific code: `dd-display/src/metal_backend.rs:304`, `dd-display/src/metal_backend.rs:1602`, `dd-display/src/metal_backend.rs:1725`.
- No `destroy_pipeline` override was found for the Metal backend.

Why this is bad:

Checked backends should expose matching cleanup semantics for every resource kind. Missing or divergent Metal cleanup can leave backend objects alive after the IR resource was destroyed, and tests that only exercise software cleanup will miss it.
