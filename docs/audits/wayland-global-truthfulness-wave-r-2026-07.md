# Wayland global truthfulness audit (wave R, 2026-07)

This is a source audit of every global currently created by the legacy `dd-display` server and the
Smithay compositor. It is not a source-shape gate: closure requires protocol round trips and observable
behavior. Versions below are the versions explicitly advertised by the legacy server; Smithay owns the
wire versions of its state objects and must be checked from a registry trace after every vendored update.

## Result

Do not remove toolkit-probe globals merely because the current host has no matching device. The Smithay
tablet and gesture implementations truthfully represent a seat with no tablet/gesture source. Conversely,
recording a hint without applying its promised host policy is not completion. The only presently safe
unadvertise candidate is the already-default-off private `surface_augmenter`; the remaining gaps should be
implemented or retained behind an existing capability gate until client traces prove absence is harmless.

| Global/group | Legacy server | Smithay compositor | Truthfulness and disposition |
|---|---|---|---|
| `wl_compositor`, `wl_subcompositor` | v4/v1; real surface, synchronized/desynchronized child composition | real Smithay v5 compositor | Keep both. Pixel tests must cover child position, stacking, sync commit and destruction. |
| `wl_shm` | v1 ARGB/XRGB import | real Smithay import | Keep. Test invalid stride/size and both formats by rendered pixels. |
| `xdg_wm_base` | v1 toplevel/popup | real shell plus declared maximize/fullscreen/minimize/menu capabilities | Keep. Smithay capabilities require host-visible state transitions, not merely configure events. |
| `wl_output` | v4, one fixed output | v4 plus `zxdg_output_manager_v1`, multi-output model | Keep; legacy is compatibility-only and lower fidelity. Trace version-gated events and test live scale/geometry changes. |
| `wl_seat` | v5 and claims pointer, keyboard **and touch** | v5 pointer/keyboard; no touch device | Fix legacy: its touch capability is higher than implemented. Stop claiming touch unless a real `wl_touch` event path exists. |
| `wp_viewporter` | v1; crop/destination state reaches composition | real Smithay state | Keep. Render source crop, destination scaling, invalid sizes and transform interaction. |
| `wl_data_device_manager` | v3 clipboard/data offers; DnD surface is narrow | Smithay clipboard/DnD plus host clipboard hooks | Keep for Chrome/GTK/Qt. Exercise guest↔guest and guest↔host MIME transfer and a complete DnD enter/motion/drop/finish trace. |
| `wp_presentation` | v1 feedback | Smithay feedback driven by presenter evidence | Keep: Chrome scheduling consumes it. Test timestamps/refresh/kind against actual presented and discarded frames. |
| `wp_cursor_shape_manager_v1` | real named host cursor mapping | Smithay named cursor route | Keep. Verify representative pointer/text/grab shapes at host boundary. |
| `zwp_linux_dmabuf_v1` | gated, v4 feedback/import | gated, v5 with feedback or v3 without | Keep gated. Registry version must follow the active import/feedback path; test valid import pixels and malformed planes/modifiers without partial state. |
| `surface_augmenter` | private, default off, debug env only | absent | Remove the legacy implementation and env switch after one Chrome startup trace confirms no bind. It has no production consumer and must never be on by default. |
| `zxdg_decoration_manager_v1` | absent | client/server decoration negotiation | Keep for GTK/Qt; verify host chrome and configure mode agree under both policies. |
| `xdg_activation_v1` | absent | token activates/raises a toplevel | Keep; test valid, stale and cross-client tokens by observed focus/raise. |
| `wp_fractional_scale_manager_v1` | absent | preferred scale plus viewporter composition | Keep; pixel-test 1.25x/1.5x with input coordinates and output migration. |
| `wp_single_pixel_buffer_manager_v1` | absent | real solid-color buffers | Keep; pixel-test color/alpha and buffer lifetime. |
| `zwp_primary_selection_device_manager_v1` | absent | guest selection through Smithay | Keep for terminals/toolkits. Add guest↔guest and middle-click/host policy tests; do not infer host bridging from object creation. |
| relative pointer + pointer constraints | absent | event injection and lock/confine state | Keep for games/3D. Test focus loss, one-shot/persistent lifetime, confined motion and relative deltas. |
| `zwp_pointer_gestures_v1` | absent | valid objects, but production injects no gestures | Keep: this is truthful no-touchpad behavior and toolkit probing depends on it. Add a host gesture bridge before claiming gesture support; retain virtual-injection protocol tests. |
| `zwp_tablet_manager_v2` | absent | valid tablet seat with zero devices | Keep: zero tablets is truthful and common. The test-only virtual device must not become a production capability claim. |
| `zwp_idle_inhibit_manager_v1` | absent | records inhibitors; exposes aggregate state | Incomplete policy. Wire aggregate transitions to a host sleep/display assertion and test assertion acquire/release, surface destruction and multiple inhibitors. |
| `wp_content_type_manager_v1` | absent | records per-surface hint | State-only but protocol-valid. Keep for negotiation; either consume the hint in presentation policy or document intentionally neutral policy. Test replacement/destruction, not source text. |
| `zxdg_exporter_v2` + `zxdg_importer_v2` | absent | Smithay handles and parent relationship | Keep. Test cross-client handle import, invalidation and host window stacking/ownership, not just handle events. |
| keyboard-shortcuts inhibit | absent | inhibitors immediately activate; compositor has no reserved guest chords | Keep. Test all keys reach the focused client while active and lifecycle/focus changes deactivate correctly. |
| `zwp_text_input_manager_v3` | absent | custom 459-line implementation with host IME seams | Keep. Test enable/commit serial ordering, preedit/commit/delete-surrounding, focus transfer and destruction with a Rust client. |

## Duplication and maintenance footprint

The legacy protocol server is a 5,249-line hand-written dispatcher. Smithay handlers total 4,122 lines;
the compositor module plus delegates compose 26 protocol globals/groups. Eleven groups are duplicated
across both paths: compositor/subcompositor, shm, xdg shell, seat, output, dmabuf, viewporter, data device,
presentation, cursor shape and the core surface/buffer lifecycle supporting them. This duplication is the
largest protocol maintenance liability: bug fixes and conformance tests must currently land twice.

Retire the legacy path only after the Smithay default passes the retained application matrix and Chrome fix
plan. Until then, put new protocol implementation exclusively in Smithay and keep legacy changes to proven
compatibility/correctness defects. After `surface_augmenter` is unadvertised permanently, delete its object
variants, dispatcher branches, request handler, env plumbing and tests together; deleting only its state
would leave a bindable broken global.

## Required evidence, in order

1. Capture `WAYLAND_DEBUG=client` registry/bind traces for unmodified Chrome, one GTK app and one Qt app on
   both compositor paths. Record offered and bound versions; compare requests through first mapped frame.
2. Add a Rust/C protocol client that binds each offered global at every supported version, sends valid and
   invalid lifecycles, and asserts protocol errors/events. Never inspect Rust source strings as a proxy.
3. Add rendered-pixel scenes for subsurfaces, popups, transforms, viewport, alpha, fractional scale and
   single-pixel buffers, plus host-observable input, clipboard, activation, decoration and idle-inhibit tests.
4. Unadvertise only when traces show no required client bind and the protocol cannot be represented
   truthfully. Run Chrome/GTK/Qt startup and interaction gates before deleting internal state.
5. On each Smithay vendor update, diff registry versions and rerun the version matrix; constructors hide
   version changes that a compile-only test cannot detect.

Dedicated performance benchmarks are not acceptance evidence for these rows. Keep tests in Rust/C and
judge closure by protocol behavior, rendered output and host-visible effects.
