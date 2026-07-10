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

## Metal Duplicate IDs And Format Fallbacks Diverge From Checked Backends

Priority: P2
Impact: stale resources or wrong texture formats can be accepted silently
Confidence: Medium-high

Evidence:

- Metal resource maps insert texture IDs directly, overwriting duplicates: `dd-display/src/metal_backend.rs:1448`.
- Texture format handling falls back to BGRA for unsupported or unexpected formats: `dd-display/src/metal_backend.rs:195`.

Why this is bad:

Other backends tend to reject invalid or duplicate resource definitions. Silent overwrite and fallback can hide guest bugs and produce frames that use stale resources or different channel formats than requested.

## Metal Shader Id Can Retain Stale MSL

Priority: P2
Impact: pipelines can keep using an old shader after id reuse
Confidence: Medium-high

Evidence:

- Shader creation does nothing when `msl_from_words` returns `None`: `dd-display/src/metal_backend.rs:1546`.

Why this is bad:

If a shader id previously held MSL and is recreated with non-MSL or empty payload data, the old library may remain in the map. Later pipelines using that id can render with stale shader code rather than failing or replacing the resource.

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
