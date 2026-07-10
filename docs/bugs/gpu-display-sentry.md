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

## GPU Software Backend Panics On Wrapping Offsets

Priority: P2
Impact: malformed IR can panic instead of returning `GpuError::OutOfBounds`
Confidence: High

Evidence:

- `write_buffer` checks `off + data.len()` without checked arithmetic: `dd-gpu/src/software.rs:203`.
- `CopyBufferToBuffer` checks `so + sz` and `d_off + chunk.len()` without checked arithmetic: `dd-gpu/src/software.rs:370`.

Why this is bad:

Large offsets such as `u64::MAX` can overflow in debug builds. The backend should return a controlled error for invalid IR, not panic.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-verifier6/target-agent6 cargo test -p dd-gpu software_backend_rejects_wrapping -- --nocapture
```

Result: panics at `software.rs` offset checks.

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

## GPU IR Length Fields Preallocate Before Validation

Priority: P2
Impact: memory/performance cliff on malformed or untrusted GPU IR
Confidence: Medium-high

Evidence:

- Vertex attributes allocate directly from decoded count: `dd-gpu/src/ir.rs:535`.
- Render pipeline vertex/color target vectors allocate directly from decoded counts: `dd-gpu/src/ir.rs:618`, `dd-gpu/src/ir.rs:623`.
- Bind group entries allocate directly from decoded count: `dd-gpu/src/ir.rs:675`.
- Render-pass color attachments allocate directly from decoded count: `dd-gpu/src/ir.rs:824`.
- Command-buffer ops allocate directly from decoded count: `dd-gpu/src/ir.rs:943`.

Why this is bad:

Malformed IR can force large `Vec::with_capacity` reservations before the decoder has proven enough bytes exist for the declared elements. This is a memory and latency cliff in the replay path.

Verification:

Add decoder tests with huge counts and minimal trailing bytes. Expected behavior: reject with `GpuError` before large allocation, ideally with a fixed maximum per vector kind.

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

## Nonzero Texture Mip Copies Alias Base Level

Priority: P1
Impact: mip uploads can overwrite base texture data
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AE-display-gpu-20260710`.

Evidence:

- `CopyBufferToTexture` includes a `mip` field, but software replay ignores it and writes `t.pixels[..need]`: `dd-gpu/src/software.rs:387`.
- Metal creates textures with mipmapping disabled: `dd-display/src/metal_backend.rs:1491`.
- Metal copy uses destination level `0`: `dd-display/src/metal_backend.rs:2030`.

Why this is bad:

A guest upload to mip level 1 should either update mip 1 or fail with a typed unsupported/bounds error. dd writes the data into base level storage, corrupting the visible texture.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AE-display-gpu-20260710-target cargo test -p dd-gpu software_backend_nonzero_mip_copy_does_not_alias_base_level -- --nocapture
```

Result: failed; base level became `[222, 173, 190, 239]` instead of remaining `[0, 0, 0, 0]`.

## Software Texture Readback Ignores `bytes_per_row`

Priority: P2
Impact: padded readbacks are silently packed
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AE-display-gpu-20260710`.

Evidence:

- `CopyTextureToBuffer` computes a tight `width * height * bpp` chunk: `dd-gpu/src/software.rs:408`.
- The write path copies rows contiguously and ignores requested row stride: `dd-gpu/src/software.rs:414`, `dd-gpu/src/software.rs:427`.

Why this is bad:

Readback buffers often have padding between rows. Packing rows tightly corrupts CPU-visible output and can make the software backend diverge from GPU semantics.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AE-display-gpu-20260710-target cargo test -p dd-gpu software_backend_copy_texture_to_buffer_honors_bytes_per_row -- --nocapture
```

Result: failed; row 1 started at byte `8`, while bytes `8..12` should have remained padding and row 1 should have started at byte `12`.

## Depth Attachments Ignore Guest Texture And Load/Store Semantics

Priority: P1
Impact: multi-pass depth rendering can use wrong contents
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AK-display-gpu-20260710`.

Evidence:

- IR carries `DepthAttachment.texture` and load semantics: `dd-gpu/src/ir.rs:268`.
- Software backend ignores render-pass depth entirely: `dd-gpu/src/software.rs:328`.
- Metal allocates/reuses an internal depth texture, does not use `da.texture`, always clears, and stores `DontCare`: `dd-display/src/metal_backend.rs:1816`.

Why this is bad:

Applications that manage depth textures across passes expect their depth attachment contents and load/store operations to matter. dd can silently render with an internal cleared depth buffer or accept missing depth resources instead of failing.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AK-display-gpu-20260710-target cargo test -p dd-gpu software_backend_rejects_missing_depth_attachment_texture -- --nocapture
```

Result: failed; missing depth texture `99` returned `Ok(())`, expected `GpuError::UnknownId`.

## Software Backend Accepts Draws With Missing Vertex Buffers

Priority: P2
Impact: headless oracle accepts invalid command streams
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AK-display-gpu-20260710`.

Evidence:

- Software `Draw` / `DrawIndexed` only increments a draw counter and does not validate bound vertex/index resources: `dd-gpu/src/software.rs:362`.
- Metal validates `SetVertexBuffer` ids and returns `UnknownId`: `dd-display/src/metal_backend.rs:1843`.

Why this is bad:

The software backend can pass command streams that the real backend rejects. That masks resource lifetime, missing-id, and id-reuse bugs in tests that rely on the software oracle.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AK-display-gpu-20260710-target cargo test -p dd-gpu software_backend_rejects_missing_vertex_buffer_on_draw -- --nocapture
```

Result: failed; missing vertex buffer `99` returned `Ok(())`, expected `GpuError::UnknownId`.

## Texture Copy Extents Ignore Texture Dimensions

Priority: P1
Impact: out-of-bounds texture copies can report success
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AO-display-gpu-20260710`.

Evidence:

- Software `CopyBufferToTexture` validates only total byte count: `dd-gpu/src/software.rs:387`.
- Software `CopyTextureToBuffer` also validates only tight byte count: `dd-gpu/src/software.rs:408`.
- Metal upload path lacks destination extent validation before copy: `dd-display/src/metal_backend.rs:2022`.

Why this is bad:

A copy region wider than the texture dimensions should return `OutOfBounds`. dd can accept the command as long as the byte count fits storage, hiding malformed streams and corrupting texture layout assumptions.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AO-display-gpu-20260710-target cargo test -p dd-gpu software_backend_rejects_copy_buffer_to_texture_extent_wider_than_texture -- --nocapture
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AO-display-gpu-20260710-target cargo test -p dd-gpu software_backend_rejects_copy_texture_to_buffer_extent_wider_than_texture -- --nocapture
```

Result: both failed with `Ok(())`; expected `GpuError::OutOfBounds` for copying width `8` into a `4x4` texture.

## Bind-Group Buffer Ranges Are Not Validated

Priority: P1
Impact: shaders can read out-of-range buffer slices
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AO-display-gpu-20260710`.

Evidence:

- Software validates only buffer existence for bind groups: `dd-gpu/src/software.rs:281`.
- Metal records offsets without checking size/liveness: `dd-display/src/metal_backend.rs:1739`.
- Metal binds those ranges later: `dd-display/src/metal_backend.rs:1907`.

Why this is bad:

Bind group buffer entries carry offset and size. Accepting a slice beyond the buffer end should fail immediately; otherwise replay can use invalid ranges or diverge between backends.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AO-display-gpu-20260710-target cargo test -p dd-gpu software_backend_rejects_out_of_bounds_bind_group_buffer_slice -- --nocapture
```

Result: failed with `Ok(())`; expected `GpuError::OutOfBounds` for `offset=12, size=8` in a 16-byte buffer.

## Multisample Texture Descriptors Are Silently Downleveled

Priority: P2
Impact: MSAA render targets lose multisample semantics
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AO-display-gpu-20260710`.

Evidence:

- Software allocation ignores `sample_count`: `dd-gpu/src/software.rs:222`.
- Metal creates a plain 2D descriptor and never applies `desc.sample_count`: `dd-display/src/metal_backend.rs:1490`.

Why this is bad:

If multisample textures are unsupported, creation should fail with a typed error. Silently creating single-sample storage changes rasterization and resolve semantics.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AO-display-gpu-20260710-target cargo test -p dd-gpu software_backend_rejects_multisample_texture_instead_of_downleveling -- --nocapture
```

Result: failed with `Ok(())`; expected rejection or real multisample allocation/resolve behavior.

## GPU Resource Usage Bits Are Ignored For Render Attachments

Priority: P1
Impact: sampled-only textures can be mutated as render targets
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AS-display-gpu-20260710`.

Evidence:

- Software render-pass setup validates attachment existence but not usage flags: `dd-gpu/src/software.rs:328`.
- Metal creates textures from descriptor usage: `dd-display/src/metal_backend.rs:1488`.
- Metal later attaches textures as render targets: `dd-display/src/metal_backend.rs:1793`.

Why this is bad:

Resource usage flags are part of the command contract. A texture created only for sampling should not be accepted as a render attachment and cleared/mutated.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AS-display-gpu-20260710/target-as cargo test -p dd-gpu poc_software_backend_rejects_render_pass_attachment_without_render_usage -- --nocapture
```

Result: failed with `Ok(())`; expected typed rejection.

## Copy Commands Ignore Copy Usage Bits

Priority: P1
Impact: invalid IR can mutate resources without copy permissions
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AS-display-gpu-20260710`.

Evidence:

- Software `CopyBufferToTexture` does not validate `COPY_SRC` / `COPY_DST` usage: `dd-gpu/src/software.rs:387`.
- Metal copy path similarly proceeds through the copy command path: `dd-display/src/metal_backend.rs:1999`.

Why this is bad:

Buffers and textures without copy usage should not be accepted by copy commands. Accepting them can hide invalid command streams and mutate resources that should be read-only for that operation.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AS-display-gpu-20260710/target-as cargo test -p dd-gpu poc_software_backend_rejects_copy_without_copy_usage -- --nocapture
```

Result: failed with `Ok(())`; expected typed rejection.

## Present Accepts Texture Size Mismatch

Priority: P2
Impact: wrong-size frames can be handed off as valid presents
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-worker-AS-display-gpu-20260710`.

Evidence:

- Software `present` checks format but accepts mismatched texture/surface dimensions: `dd-gpu/src/software.rs:439`.

Why this is bad:

Presenting a texture whose dimensions differ from the surface can hide swapchain bugs and produce scaled, cropped, or otherwise wrong frame handoff.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-worker-AS-display-gpu-20260710/target-as cargo test -p dd-gpu poc_software_backend_rejects_present_size_mismatch -- --nocapture
```

Result: failed through the success path; expected rejection or an explicit size-incompatibility signal.

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

## 3D/Depth Texture Descriptors Are Flattened

Priority: P2
Impact: non-2D textures silently lose dimensional semantics
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-bd2-worktree`.

Evidence:

- Software backend allocates `bytes_per_texel * width * height`, ignoring `TextureDesc.depth` and `TextureDesc.dim`: `dd-gpu/src/software.rs:222`.
- Metal creates a 2D descriptor from width/height: `dd-display/src/metal_backend.rs:1491`.

Why this is bad:

If 3D/depth textures are unsupported, creation should fail. Silently flattening them lets command streams proceed with the wrong storage shape and backend semantics.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-bd2-target cargo test -p dd-gpu software_backend_rejects_non_2d_texture_descriptors_instead_of_flattening -- --nocapture
```

Result: failed because `TextureDim::D3` with `depth = 2` was accepted.

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

## Zero-Sized GPU Textures Are Accepted

Priority: P2
Impact: backends silently create a different texture size
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-bg2-display-gpu-20260710`.

Evidence:

- Software texture creation accepts zero width/height: `dd-gpu/src/software.rs:222`.
- Metal path uses `desc.width.max(1)`: `dd-display/src/metal_backend.rs:1491`.

Why this is bad:

Zero-sized texture descriptors should return a typed error. Silently creating a `1`-wide Metal texture or accepting zero in software creates backend divergence and invalid resource state.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-bg2-display-gpu-20260710-target cargo test -p dd-gpu software_backend_rejects_zero_sized_textures -- --nocapture
```

Result: failed because `create_texture(... width=0 ...)` returned `Ok(())`.

## Wrapping Bind-Group Offsets Panic During Dispatch

Priority: P2
Impact: malformed bind groups can panic the software backend
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-bg2-display-gpu-20260710`.

Evidence:

- Software dispatch computes buffer slices with unchecked addition: `dd-gpu/src/software.rs:120`.
- Bind group creation accepts the offset/size pair: `dd-gpu/src/software.rs:281`.
- Metal records bind group offsets without checking size/liveness: `dd-display/src/metal_backend.rs:1739`.

Why this is bad:

A bind group entry with `offset=u64::MAX, size=1` should reject as out-of-bounds. dd accepts it and panics later during dispatch due to integer overflow.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-bg2-display-gpu-20260710-target cargo test -p dd-gpu software_backend_dispatch_rejects_wrapping_bind_group_offset_without_panic -- --nocapture
```

Result: panicked at `dd-gpu/src/software.rs:120:20` with `attempt to add with overflow`.

## Oversized Texture Descriptors Panic Software Backend

Priority: P1
Impact: malformed texture dimensions can panic before typed rejection
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-bl2`.

Evidence:

- Software texture creation multiplies `bpt * width * height` without checked arithmetic: `dd-gpu/src/software.rs:224`.

Why this is bad:

Texture descriptors with huge dimensions should return a typed `GpuError`, not panic in the backend. A panic can take down tests or replay processes handling malformed IR.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-bl2-target cargo test -p dd-gpu software_backend_rejects_texture_dimension_overflow_without_panic -- --nocapture
```

Result: panicked at `dd-gpu/src/software.rs:224:17` with `attempt to multiply with overflow`.

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

## GPU `SetViewport` Invalid Depth Range Is Accepted

Priority: P1
Impact: invalid command streams replay instead of failing validation
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-BO2-20260710`.

Evidence:

- GPU IR carries `SetViewport` depth values: `dd-gpu/src/ir.rs:297`.
- Software replay accepts the viewport command: `dd-gpu/src/software.rs:429`.
- Metal replay forwards it to backend viewport state: `dd-display/src/metal_backend.rs:1956`.

Why this is bad:

Viewport depth must be valid before replay. Accepting `min_depth=1.0, max_depth=0.0` lets malformed GPU IR proceed with backend-specific behavior instead of returning a typed validation error.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-BO2-target cargo test -p dd-gpu software_backend_rejects_invalid_viewport_depth_range -- --nocapture
```

Result: `min_depth=1.0, max_depth=0.0` returned `Ok(())`.

## Zero-Mip Texture Descriptors Are Accepted

Priority: P2
Impact: invalid texture descriptors enter backend state
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-BO2-20260710`.

Evidence:

- Texture descriptors carry `mip_levels`: `dd-gpu/src/ir.rs:132`.
- Software texture creation accepts the descriptor: `dd-gpu/src/software.rs:222`.
- Metal texture creation consumes `mip_level_count`: `dd-display/src/metal_backend.rs:1490`.

Why this is bad:

Textures should have at least one mip level. Accepting `mip_levels: 0` creates invalid resource state and backend divergence.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-BO2-target cargo test -p dd-gpu software_backend_rejects_zero_mip_level_texture_descriptors -- --nocapture
```

Result: `mip_levels: 0` returned `Ok(())`.

## Failed GPU Submit Leaves Partial Resource Mutations

Priority: P1
Impact: failed command streams can still mutate textures/buffers
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-agent-gpu-lifecycle-20260710`.

Evidence:

- Software submit executes commands in order and mutates resources during a render-pass clear: `dd-gpu/src/software.rs:321`, `dd-gpu/src/software.rs:328`.
- A later invalid copy can return an error after those mutations: `dd-gpu/src/software.rs:370`.

Why this is bad:

If submit returns an error, callers should not observe partial side effects unless the API explicitly documents partial execution. Current behavior can expose a clear or write even though the submit failed.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-agent-gpu-lifecycle-20260710-target cargo test -p dd-gpu software_backend_failed_submit_does_not_partially_mutate_resources -- --nocapture
```

Result: failed submit left `[255, 0, 0, 255]` visible where `[0, 0, 0, 0]` was expected.

## Bind Groups Can Mutate Reused Resource IDs

Priority: P1
Impact: stale bind groups can write into unrelated new buffers
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-agent-gpu-lifecycle-20260710`.

Evidence:

- Bind group creation validates resources once and stores raw descriptor IDs: `dd-gpu/src/software.rs:281`.
- Dispatch later resolves those IDs dynamically: `dd-gpu/src/software.rs:112`, `dd-gpu/src/software.rs:136`.
- Resource generation tracking exists but is not used to reject stale bind-group references: `dd-gpu/src/id.rs:43`.

Why this is bad:

Destroying buffer ID `1` and reusing that ID should not let an old bind group write into the new buffer. Either dependencies must keep resources alive or generation checks must reject stale references.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-agent-gpu-lifecycle-20260710-target cargo test -p dd-gpu software_backend_rejects_stale_bind_group_after_buffer_id_reuse -- --nocapture
```

Result: stale bind group was accepted and mutated the reused buffer to `[17, 34, 51, 68]`.

## Command Encoder Pass Sequencing Is Not Validated

Priority: P2
Impact: malformed GPU command streams pass local validation
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-agent-gpu-lifecycle-20260710`.

Evidence:

- Software submit handles render and copy commands without tracking a current pass: `dd-gpu/src/software.rs:326`, `dd-gpu/src/software.rs:366`.
- Dispatch is accepted based on pipeline/bind-group state, not compute-pass state: `dd-gpu/src/software.rs:429`.

Why this is bad:

`Dispatch` outside `BeginComputePass` / `EndComputePass` should return a typed validation error. Accepting it lets malformed streams diverge from normal GPU command encoding rules.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-agent-gpu-lifecycle-20260710-target cargo test -p dd-gpu software_backend_rejects_dispatch_outside_compute_pass -- --nocapture
```

Result: dispatch outside a compute pass was accepted.

## Invalid Texture Copy Row Pitch Is Accepted

Priority: P1
Impact: invalid texture uploads can pass validation or diverge across backends
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-bq2`.

Evidence:

- Software `CopyBufferToTexture` ignores `bytes_per_row` and copies a tight `width * height * bpp` byte range: `dd-gpu/src/software.rs:387`.
- Metal uses `bytes_per_row` for the blit and only checks `bytes_per_row * height`: `dd-display/src/metal_backend.rs:1999`.

Why this is bad:

A 2x2 RGBA upload needs at least 8 bytes per row. Accepting `bytes_per_row=4` should be a typed validation error; instead software accepts it and Metal can receive an invalid layout.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/target-audit-gpu-bq2 cargo test -p dd-gpu software_backend_rejects_short_texture_copy_row_pitch -- --nocapture
```

Result: returned `Ok(())` for a too-small row pitch.

## Pipeline Color-Target Format Can Diverge From Attachment

Priority: P1
Impact: render pipelines can draw into incompatible attachment formats
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-bq2`.

Evidence:

- Software stores render pipelines without color target formats: `dd-gpu/src/software.rs:37`, `dd-gpu/src/software.rs:266`.
- Submit validates attachment existence but not pipeline format compatibility: `dd-gpu/src/software.rs:328`, `dd-gpu/src/software.rs:354`.
- Metal bakes the pipeline color pixel format from `ColorTargetState`: `dd-display/src/metal_backend.rs:1653`.

Why this is bad:

A render pipeline targeting RGBA8 should not be used with a BGRA8 render attachment. Software accepts the mismatch, creating backend divergence and invalid render state.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/target-audit-gpu-bq2 cargo test -p dd-gpu software_backend_rejects_pipeline_attachment_format_mismatch -- --nocapture
```

Result: returned `Ok(())` for an RGBA8 pipeline used with a BGRA8 attachment.

## Software Fence Waits Fabricate Completion

Priority: P2
Impact: waits can complete without a prior signal
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-bq2`.

Evidence:

- Backend trait defines `wait_fence` as waiting until the fence reaches a value: `dd-gpu/src/backend.rs:126`.
- Software `wait_fence` mutates the fence value to the requested wait value and returns success: `dd-gpu/src/software.rs:314`.

Why this is bad:

Fence waits should observe completion, not create it. Fabricating a signal can hide missing submissions or ordering bugs.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/target-audit-gpu-bq2 cargo test -p dd-gpu software_backend_wait_fence_does_not_fabricate_signal -- --nocapture
```

Result: unsignaled wait returned `Ok(())`.

## Vertex And Index Draw Range Validation Is Missing

Priority: P1
Impact: draw commands can read beyond bound vertex or index buffers
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-validation`.

Evidence:

- Software render pipelines discard vertex layout detail: `dd-gpu/src/software.rs:266`.
- `Draw` and `DrawIndexed` only increment counters: `dd-gpu/src/software.rs:362`.
- Metal records index buffer id/offset without validating the id or byte range: `dd-display/src/metal_backend.rs:1936`, `dd-display/src/metal_backend.rs:1982`.

Why this is bad:

Backends should reject draws whose vertex/index ranges exceed bound buffers. dd accepts 2 vertices at stride 16 from a 16-byte buffer, and 2 `U16` indices from a 2-byte index buffer.

Isolated proof:

```sh
cd /Users/x/dd/dd-audit-gpu-validation
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-gpu-validation-target cargo test -p dd-gpu software_backend_rejects -- --nocapture
```

Result: vertex and indexed draw range tests returned `Ok(())`, expected `Err(OutOfBounds)`.

## Invalid Shader Modules Are Accepted

Priority: P1
Impact: invalid shaders can reach pipeline creation or fall back to builtins
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-validation`.

Evidence:

- Software treats arbitrary non-kernel payloads, including empty bytes, as opaque SPIR-V: `dd-gpu/src/software.rs:249`, `dd-gpu/src/software.rs:266`.
- Metal can return success for non-MSL/empty payloads without installing a shader: `dd-display/src/metal_backend.rs:1546`.
- Render pipeline creation can fall back to builtin shaders: `dd-display/src/metal_backend.rs:1628`.

Why this is bad:

Invalid shader modules should fail before pipeline creation or draw. Accepting empty modules can hide shader upload failures and make Metal use fallback shaders.

Isolated proof:

```sh
cd /Users/x/dd/dd-audit-gpu-validation
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-gpu-validation-target cargo test -p dd-gpu software_backend_rejects_empty_shader_module -- --nocapture
```

Result: `create_shader(ShaderId(1), &[])` returned `Ok(())`.

## Vertex Attribute Layout Validation Is Missing

Priority: P2
Impact: impossible vertex layouts are accepted
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-validation`.

Evidence:

- Software stores only `Pipeline::Render`: `dd-gpu/src/software.rs:266`.
- Metal forwards offset/stride into the vertex descriptor and clamps stride with `max(4)` instead of rejecting: `dd-display/src/metal_backend.rs:1684`.

Why this is bad:

An attribute at `offset=16` with `stride=8` cannot fit in each vertex. Pipeline creation should reject it instead of accepting impossible layout state.

Isolated proof:

```sh
cd /Users/x/dd/dd-audit-gpu-validation
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-gpu-validation-target cargo test -p dd-gpu software_backend_rejects_vertex_attribute_outside_stride -- --nocapture
```

Result: invalid vertex attribute layout was accepted.

## Storage Textures Can Be Bound As Sampled Textures

Priority: P1
Impact: storage access mode is absent, so invalid bindings are accepted
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-gpu-tex-audit`.

Evidence:

- `BindResource::Texture` has no sampled/storage/access-mode distinction: `dd-gpu/src/ir.rs:227`.
- Software only checks that the texture id exists: `dd-gpu/src/software.rs:281`.
- Metal records every texture binding as sampled and maps non-sampled textures without `ShaderWrite`: `dd-display/src/metal_backend.rs:1488`, `dd-display/src/metal_backend.rs:1739`.

Why this is bad:

A storage-only texture should not be accepted as a sampled texture binding. Without explicit binding/access mode, storage texture correctness and backend behavior are undefined.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-gpu-tex-audit-target cargo test -p dd-gpu software_backend_rejects_sampling_storage_only_texture -- --nocapture
```

Result: storage-only texture binding returned success.

## Same-Pass Sampled Render-Target Hazard Is Accepted

Priority: P1
Impact: a texture can be read and written in the same render pass
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-gpu-tex-audit`.

Evidence:

- Software accepts color attachments and bound sampled textures independently: `dd-gpu/src/software.rs:328`, `dd-gpu/src/software.rs:358`.
- Metal binds textures and render targets without hazard validation: `dd-display/src/metal_backend.rs:1793`, `dd-display/src/metal_backend.rs:1920`.

Why this is bad:

A texture should not be sampled while it is also writable as the active color attachment in the same pass. dd accepts the hazard and returns success.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-gpu-tex-audit-target cargo test -p dd-gpu software_backend_rejects_sampled_render_target_write_hazard -- --nocapture
```

Result: submit returned `Ok(())` for same-pass sampled/render-target use.

## Destroyed Sampled Textures Remain Usable Through Bind Groups

Priority: P1
Impact: stale texture references can survive destruction
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-readback-lifetime`.

Evidence:

- Bind group creation validates texture IDs once and stores raw IDs: `dd-gpu/src/software.rs:281`.
- `SetBindGroup` validates only the bind-group ID: `dd-gpu/src/software.rs:358`.
- `Draw` never revalidates bound texture resources: `dd-gpu/src/software.rs:362`.

Why this is bad:

Destroying a texture should invalidate dependent bindings or keep the resource alive explicitly. dd lets a bind group reference texture `1`, destroys texture `1`, then still draws successfully without recreating the texture.

Isolated proof:

```sh
cd /Users/x/dd/dd-audit-gpu-readback-lifetime
CARGO_TARGET_DIR=/Users/x/dd/target-gpu-readback-lifetime cargo test -p dd-gpu software_backend_rejects_destroyed_texture_bound_in_bind_group -- --nocapture
```

Result: submit returned `Ok(())`; expected a stale texture reference error such as `UnknownId`.

## Texture-To-Buffer Readback Accepts Unaligned Offsets

Priority: P2
Impact: readbacks can write texels at invalid byte alignment
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-readback-lifetime`.

Evidence:

- `CopyTextureToBuffer` includes `dst_offset`: `dd-gpu/src/ir.rs:355`.
- Software copies texel bytes directly into that offset without texel/block alignment validation: `dd-gpu/src/software.rs:408`.

Why this is bad:

For formats such as `Rgba8Unorm`, destination offsets should align to texel/block size. dd accepts `dst_offset=1` and writes a pixel starting at byte 1.

Isolated proof:

```sh
cd /Users/x/dd/dd-audit-gpu-readback-lifetime
CARGO_TARGET_DIR=/Users/x/dd/target-gpu-readback-lifetime cargo test -p dd-gpu software_backend_rejects_unaligned_texture_to_buffer_offset -- --nocapture
```

Result: submit returned `Ok(())`; expected a typed validation error.

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

## Partial GPU Command Ring Frames Can Panic And Corrupt Queue State

Priority: P1
Impact: malformed or short command frames can panic or corrupt the ring buffer
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-serde-20260710`.

Evidence:

- `pop_frame()` consumes the length prefix before checking that the full body exists: `dd-gpu/src/ring.rs:74`.
- Ring reads decrement length without guarding incomplete bodies: `dd-gpu/src/ring.rs:57`.

Why this is bad:

A queue reader should leave incomplete frames buffered until the body arrives, or return a typed decode error. dd consumes the header and reads `n` bytes anyway; debug builds panic on length underflow, and release builds risk corrupting queue state or fabricating body bytes.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-gpu-serde-20260710-target cargo test -p dd-gpu pop_frame_waits_for_complete_body -- --nocapture
```

Result: panic at `dd-gpu/src/ring.rs:57` with `attempt to subtract with overflow`.

## CUDA Launches Leak Transient GPU Resources

Priority: P1
Impact: every launch can retain parameter buffers and bind groups
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-cleanup-20260710`.

Evidence:

- CUDA launch creates a `kernel-params` buffer and bind group per launch: `dd-gpu/src/cuda.rs:337`, `dd-gpu/src/cuda.rs:353`.
- The launch path submits work and returns without destroying those transient resources.

Why this is bad:

Repeated CUDA launches should not grow backend resource tables indefinitely. dd allocates launch-local resources and never emits matching destroy operations, creating a resource leak on long-running GPU workloads.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-gpu-cleanup-20260710-target cargo test -p dd-gpu cuda_launch_releases_transient_parameter_resources -- --nocapture
```

Result: `created_param_buffers = 8`, `destroyed_buffers = 0`, and no bind-group destroys.

## Length-Prefixed Command Frames Accept Trailing Bytes

Priority: P2
Impact: malformed framed GPU commands can be accepted with discarded payload
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-serde-20260710`.

Evidence:

- `Decoder::frame()` decodes the inner body without checking that the inner decoder reached EOF: `dd-gpu/src/wire.rs:159`.
- Command frame decode is exposed through `dd-gpu/src/lib.rs:193`.

Why this is bad:

Framed decode should reject bodies that contain extra bytes after a valid command. Accepting `CreateFence` plus a trailing byte normalizes malformed input and can desynchronize strict producers/consumers.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-gpu-serde-20260710-target cargo test -p dd-gpu framed_command_decode_rejects_trailing_bytes -- --nocapture
```

Result: the test failed because a framed command body accepted a trailing byte.

## Non-Canonical Boolean Wire Values Are Normalized

Priority: P2
Impact: malformed GPU IR becomes indistinguishable from canonical true values
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-serde-20260710`.

Evidence:

- Boolean decode treats any nonzero byte as true: `dd-gpu/src/wire.rs:134`.
- Render pipeline decode consumes boolean fields from command IR: `dd-gpu/src/ir.rs:616`.

Why this is bad:

Only `0` and `1` should be valid boolean encodings. dd accepts byte `2` as `true`, hiding malformed payloads and allowing producer bugs to become silent behavior changes.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-gpu-serde-20260710-target cargo test -p dd-gpu decode_rejects_noncanonical_bool_fields -- --nocapture
```

Result: non-`0`/`1` boolean data decoded as `true`.

## ResourceTable Keeps Generation Metadata Forever

Priority: P2
Impact: destroy/recreate loops leak per-id generation entries
Confidence: High

Verification status: Proven in isolated worktree `/Users/x/dd/dd-audit-gpu-cleanup-20260710`.

Evidence:

- Resource allocation and destroy preserve generation metadata in `ResourceTable`: `dd-gpu/src/id.rs:55`, `dd-gpu/src/id.rs:72`, `dd-gpu/src/id.rs:95`.

Why this is bad:

After resources are destroyed, live resource state should shrink or reuse bounded metadata. dd can have no live resources while retaining unbounded generation entries, which is a memory leak under churn-heavy workloads.

Isolated proof:

```sh
CARGO_TARGET_DIR=/Users/x/dd/dd-audit-gpu-cleanup-20260710-target cargo test -p dd-gpu destroyed_unique_ids_do_not_leave_unbounded_generation_entries -- --nocapture
```

Result: no live resources remained, but `gens.len() == 1024`.

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
