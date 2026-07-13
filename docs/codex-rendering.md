# Rendering completion backlog

Status: current tree, 2026-07-13. Reconciled against merged main after the rendering-cluster merges: 24 rows were
closed (real merged commit + passing behavioral test each — git records them) and removed; the 7 rows below are the
genuinely-open residuals.

This is a live engineering backlog, not implementation history. Git records completed work. Remove a row once a
Rust or C behavioral test drives the relevant public ABI, protocol, transport, backend, timing path, or pixels and
the implementation is merged. Symbol inventories and tests that search source files are audit aids, not proof.

## Scope and architecture

```text
guest EGL/GLES or Vulkan
        -> dd-shim-{gl,vk}
        -> dd-shim-common transport
        -> dd-gpu IR
        -> Metal or wgpu executor
        -> IOSurface/dmabuf
        -> dd-display or Smithay dd-compositor
        -> Cocoa/Metal presentation
```

The supported default profiles are GLES 2.0, EGL 1.4, and a truthfully reduced Vulkan 1.2 surface. Exported symbols
outside those coherent slices may be partial or unsupported. GLES 3 remains opt-in and incomplete. A successful
shim call is insufficient if the IR, selected executor, compositor, or presenter cannot preserve its semantics.

Completion means all of the following for the advertised profile:

- ABI and capability claims are truthful.
- Object, error, synchronization, memory, and lifetime semantics are observable through public calls.
- IR retains every required field and rejects unsupported work before partial mutation.
- Metal, wgpu, and software behavior agrees where each backend advertises support.
- The compositor preserves pixels, ordering, transforms, input, output routing, feedback, and buffer ownership.
- Named unmodified GUI workloads pass repeatable live journeys; skipped required hardware gates fail the requested run.

## Remaining behavioral gaps

States: `missing` means no usable implementation; `contradictory` means current behavior makes a false claim or has
incompatible paths; `partial` means a real slice exists but the stated residual remains. Completed rows are deleted.

<!-- rendering-gap-ledger:start -->
| Required behavioral regression | State | Current residual | Evidence required to close |
|---|---|---|---|
| `vk_transfer_commands_preserve_every_region_subresource_and_layout` | partial | The shim lowers every transfer region/subresource/layout with atomic validation (tested), and the wgpu executor performs real per-sample multisample resolve (`3933cffa`); the Metal executor still returns `Unsupported` for a genuine >1-sample resolve, so `dd-display/tests/metal_resolve.rs` skips that path | Implement real multisample resolve on the Metal executor, then add broader dimensions/layers and live pixel parity |
| `opt_in_gles3_has_real_implementations_for_every_mandatory_command` | partial | Real ES3 object families (query, sampler, transform-feedback, UBO) and ~67 mandatory commands have hand-written bodies with passing lifecycle tests, but ~63 commands remain non-full (compute/program-pipeline/indirect/image/base-vertex, mostly ES3.1/3.2) and no census asserts the full mandatory surface | Complete the remaining ES3 mandatory commands and add a 246-command census gate |
| `gles_draw_calls_validate_all_inputs_before_snapshot_or_recording` | partial | Mapped-buffer rejection and the negotiated `MAX_VERTEX_ATTRIBS` limit are validated before recording (tested); instanced draws collapse to a single instance because the dd-gpu IR lacks `instance_count`/`base_vertex` | Add faithful instanced/base-vertex IR fields and their validation |
| `gles_sync_objects_track_real_submission_completion_and_wait_results` | partial | The submission-serial lifecycle is coupled to a real frame submit+ack (tested), but only through the synchronous `DD_IR_DUMP` host-tool stand-in | Add asynchronous accepted/completed ACKs against a live executor and prove backend parity |
| `gles_query_objects_track_targets_availability_and_asynchronous_results` | partial | Real typed-target query lifecycle, name validation, `CURRENT_QUERY`, availability-on-completion, and reuse/nesting errors landed (`57de9e74`) with a passing test, but there is no backend query execution (the result is a truthful zero with no occlusion backend) | Add query IR, negotiated capabilities, resolve serials, and backend execution |
| `cocoa_presenter_reports_actual_drawable_presentation_time_and_refresh` | partial | Metal retains the drawable through command completion and reports valid measured `presentedTime`, serial, and target-screen refresh, with unit tests proving ordering and no fabricated vsync (`4baed768`); visible-device evidence is absent | Run a multi-frame visible macOS drawable ordering/refresh journey |
| `x11_only_gui_apps_have_an_xwayland_bridge` | partial | The feature-independent XWayland window model (adopt/withdraw → present with title + focus + input through the native path) is composed and unit-tested (`85efe9be`); the supervised XWayland server path (Xwayland spawn, XWM, XWaylandShell) is behind an offline-unbuildable cargo feature and runs no supervised journey | Build and run the supervised XWayland input, clipboard, and rendering journey |
<!-- rendering-gap-ledger:end -->

## Application and Chrome plan

Low-level fd, futex, eventfd, epoll, bootstrap-handle, EGL, and Vulkan probes do not prove Chromium. Keep the Chrome
plan until an unmodified multi-process run shows browser and renderer content, GPU-process recovery, input, menus,
resize/scale, and clean shutdown. The existing epoll re-prime WIP must not merge from timeout-only evidence: reproduce
the blank-content failure, capture process/IPC/GPU state, demonstrate the causal lost-wakeup sequence, then run a
bounded repeated journey with screenshots and resource-growth checks. Also retain live GTK3/4, Qt5/6, SDL, GLFW,
Electron, and Firefox journeys, recording compositor and executor for every result.

## Reference and implementation policy

- Agents must read `.dev/AGENTS.local.md` and the nearest `AGENTS.md`, work in isolated worktrees, and commit focused
  changes for manager review.
- Tests stay in Rust or C and exercise behavior. Do not read implementation source to assert that names or snippets
  exist.
- Use vendored Smithay protocol code for compositor behavior and pinned Khronos registries for ABI inventories.
- Use vendored MoltenVK as a semantic reference for Vulkan ownership, WSI, synchronization, descriptors, and Metal
  translation, but port the model into Rust rather than copying Objective-C++ structure blindly.
- Capability reporting comes from the negotiated intersection of shim, IR, executor, compositor, and presenter.
- Preserve the rebrand goal: public names, diagnostics, environment variables, docs, binaries, and guest-visible
  surfaces must converge on the current `dd` identity; do not reintroduce legacy branding while landing fixes.
- After every merge, rerun focused tests, remove fully proven rows, and rewrite partial rows to state only the residual.
