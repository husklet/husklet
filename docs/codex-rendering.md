# Rendering completion backlog

Status: current tree, 2026-07-12.

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

The supported default profiles are GLES 2.0, EGL 1.4, and a truthfully reduced Vulkan 1.0 surface. Exported symbols
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
| `vk_abi_manifest_contains_every_core_command_in_the_pinned_registry` | missing | ABI registry trails pinned Khronos XML by 19 Vulkan 1.4 core commands | Regenerate types/ABI, review signatures, and pass loader plus normal census |
| `vk_advertised_core_has_real_implementations_for_every_mandatory_command` | partial | Vulkan 1.0 still has 55/137 mandatory generated failures | Implement advertised semantics and pass the applicable CTS subset |
| `vk_image_layout_barriers_track_subresources_and_queue_ownership` | partial | Legacy color barriers track mip/layer state only on one queue | Cover sync2, aspects, cross-queue ownership, and backend hazards |
| `vk_transfer_commands_preserve_every_region_subresource_and_layout` | partial | Validated 2D color copies/blits/clears exist; resolve and broader dimensions/layers do not | Rich ABI/software fixtures and parity on both hardware executors |
| `vk_shader_modules_validate_spirv_entries_specialization_and_interfaces` | partial | Structural/reflection checks cover only the supported parser vocabulary | Expand SPIR-V types, descriptors, push constants, interfaces, and diagnostics |
| `opt_in_gles3_has_real_implementations_for_every_mandatory_command` | partial | Opt-in GLES3 has 112/246 mandatory stubs | Complete coherent ES3 groups and relevant CTS |
| `gles_pixel_store_and_texture_upload_validation_is_atomic_and_checked` | partial | Checked 2D byte uploads exist | Add compressed/3D formats, complete mip/layer rules, and conversions |
| `gles_framebuffer_completeness_reflects_attachment_state_and_blocks_draws` | partial | Color-only FBO validation exists | Add depth/stencil/layer/sample compatibility and read/blit guards |
| `gles_draw_calls_validate_all_inputs_before_snapshot_or_recording` | partial | Core array/index validation exists | Add mapped state, negotiated limits, and faithful instance/base-vertex IR |
| `gles_readpixels_validates_pack_layout_and_preserves_output_on_error` | partial | Color subset and bounded PBO reads are checked | Add default-FB readback, conversions, mapped PBOs, and client-size contract |
| `gles_sync_objects_track_real_submission_completion_and_wait_results` | partial | Shim-local submission serial lifecycle exists | Add cross-process accepted/completed ACKs and asynchronous backend parity |
| `gles_query_objects_track_targets_availability_and_asynchronous_results` | missing | No typed target lifecycle, readiness, or backend results | Add query IR, negotiated capabilities, resolve serials, and backend execution |
| `executor_reconnect_replays_complete_residency_or_reports_api_loss` | partial | Bounded ACKed residency replay exists | Kill a live executor and prove recovered pixels plus bounded resources |
| `executor_enforces_every_negotiated_limit_before_decoding_or_allocating` | partial | Shared replay limits validate core resources | Negotiate backend alignment and compiled-cache limits |
| `executor_accounts_cumulative_residency_and_object_counts_per_connection` | partial | Core ids and journal bytes are budgeted | Charge compiled caches, surfaces, fences, external allocations, and ownership |
| `compositor_surface_teardown_reclaims_cpu_gpu_window_and_fence_state` | partial | CPU/cache/callback/window cleanup is idempotent | Reclaim client-owned executor resources and in-flight GPU fences |
| `compositor_enforces_per_client_render_resource_budgets` | partial | Surfaces, callbacks, repacks, and shm bytes are charged | Charge fds, imports, presenter objects, and executor allocations |
| `compositor_releases_buffers_only_after_the_last_cpu_or_gpu_use` | partial | shm copy completion is exact; zero-copy lacks GPU completion | Add presenter completion tokens and out-of-order retirement tests |
| `compositor_routes_each_surface_through_its_actual_output_membership` | partial | Explicit membership and selected-output routing exist | Add geometry intersection and two-window live Wayland journey |
| `compositor_output_hotplug_migrates_surfaces_and_reconfigures_scale` | partial | Withdrawal/fallback migration exists | Wire host display notifications and fullscreen configure ordering |
| `software_backend_applies_srgb_transfer_functions_around_filtering_and_blending` | contradictory | Filtering/blending still lacks a complete linear-light execution path | Pixel tests for decode-filter/blend-encode; copies remain bit-preserving |
| `cocoa_presenter_reports_actual_drawable_presentation_time_and_refresh` | missing | No drawable completion timestamp or target refresh | Presented-handler plumbing and live macOS ordering test |
| `compositor_negotiates_surface_color_and_converts_to_the_target_output_profile` | missing | No surface color description, output profile, or HDR policy | Color protocol plus linear composition and ICC/HDR fixtures |
| `compositor_honors_input_and_opaque_regions_through_surface_transforms` | partial | Logical input-region hit testing exists; opaque regions are unused | Add conservative occlusion/damage and transformed live Wayland journey |
| `compositor_minimize_and_occlusion_control_native_visibility_and_frame_pacing` | partial | Internal hidden pacing exists | Wire AppKit notifications and run protocol-to-host reveal journey |
| `compositor_explicit_sync_waits_acquire_before_sampling_and_releases_after_gpu_completion` | missing | Internal Metal ordering is not a Wayland fence contract | Add explicit-sync/syncobj state and Linux-fence/MTLSharedEvent bridge |
| `dmabuf_feedback_serializes_an_explicit_linux_u64_device_id` | partial | Rust/C wire parsers exist but macOS guest bridge run is absent | Run recvmsg/mmap and C guest probes through the real bridge |
| `dmabuf_feedback_advertises_only_pairs_that_the_importer_can_accept` | partial | Narrow dd-tagged pairs exist without allocation generations | Share authenticated allocation metadata and run GPU-backed guest probes |
| `compositor_validates_dmabuf_planes_flags_and_backing_metadata_before_success` | partial | Plane/fd/IOSurface metadata is checked without allocation generation | Authenticate generations and add stale-id C protocol regression |
| `smithay_shm_pool_validation_prevents_oversized_mapping_truncation_and_sigbus_escape` | partial | Bounds, quotas, and isolated SIGBUS handling exist | Run linked compositor/mac gates and sustained truncate/grow isolation stress |
| `x11_only_gui_apps_have_an_xwayland_bridge` | missing | No XWayland/XWM/GLX path | Supervised XWayland input, clipboard, and rendering journey |
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
