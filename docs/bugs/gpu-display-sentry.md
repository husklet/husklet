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

