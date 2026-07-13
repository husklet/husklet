# dd rendering and shim completeness audit

Status: current-tree rendering audit, 2026-07-12.

This is a **living audit**. Source inspection and symbol census are useful review inputs but are not behavioral
proof. Remove a gap only after a Rust/C harness drives the relevant ABI, protocol, socket, backend, timing path or
pixels on the current tree. Rendering work lands frequently; validate claims against code, generated inventories,
recent commits, crate tests and the applicable live matrix before editing this file.

This document answers four separate questions that are easy to conflate:

1. Does a guest library export the symbols an application expects?
2. Do those symbols implement the API's observable semantics?
3. Can the shared `dd-gpu` IR express the work produced by the shim?
4. Can a selected host executor render that IR into a buffer the selected compositor presents?

Today the answer is not uniformly “yes”. The project has a functional accelerated slice, but is not
surface-complete in the behavioral sense for GLES/EGL or Vulkan, and its alternate compositor/backend
combination is not fully wired. Generated default-return stubs make applications link and make proc-address
lookups succeed; they do not constitute API support.

## 1. Current pipeline and ownership

```text
Linux application
  ├─ EGL/GLES ── dd-shim-gl ─────┐
  ├─ Vulkan ─── dd-shim-vk ──────┤
  └─ CUDA ───── dd-shim-cuda/rt ─┤
                                  v
                    dd-shim-common transport
                                  v
                         shared dd-gpu IR
                                  v
             ┌────────────────────┴───────────────────┐
             │                                        │
     dd-display MetalBackend                 dd-gpu-wgpu WgpuBackend
       (default executor)                    (`DD_GPU_BACKEND=wgpu`)
             │                                        │
             └──────── IOSurface / surface id ────────┘
                                  v
              legacy dd-display Wayland compositor
                  or dd-compositor/Smithay
                                  v
                         Cocoa/Metal presentation
```

The diagram describes the intended composition, not every validated combination. The original Smithay path did
bypass executor startup when `dd-display` exec'd `dd-compositor`; commit `b2d5af7b` now starts the shared executor
inside `dd-compositor` before either native or headless mode, honors `DD_GPU_BACKEND`, and diagnoses accelerated
imports when no executor is available. A Rust wiring gate proves those source-level invariants. This is
**implemented/unverified**, not closed: live accelerated Smithay + IOSurface presentation and startup readiness
still need the device-level gates below.

Authoritative source areas:

- API manifests and generated fallbacks: `dd-shim-{gl,vk,cuda,cudart}/registry` and each `build.rs`.
- Guest state/lowering: `dd-shim-gl/src`, `dd-shim-vk/src`, `dd-shim-cuda/src`.
- Wire contract: `dd-gpu/src/ir.rs`, `wire.rs`, and `dd-shim-common/src/transport.rs`.
- Executors: `dd-display/src/metal_backend.rs` and `dd-gpu-wgpu/src/backend.rs`.
- Presentation: `dd-display/src/server.rs`, `present_cocoa.rs`, and `dd-compositor/src`.

## 2. Surface census: exported is not implemented

Counts below come from the checked-in manifests and `IMPLEMENTED` arrays, not from marketing-level API
versions. The GL and Vulkan library tests prove the census identity
`implemented + generated_stubs == exported`; they do not prove the implemented entries are spec-complete.

| Guest surface | Exported | Hand-written | Generated stubs | What the number proves |
|---|---:|---:|---:|---|
| GLES 2.0–3.2 + EGL 1.0–1.5 (`dd-shim-gl`) | 402 | 218 full | **106 partial / 78 unsupported** | ABI surface exists; inventory distinguishes semantic status |
| Vulkan core + extensions (`dd-shim-vk`) | 693 | 94 | **599** | Loader can resolve the names; only 14% has a body |
| CUDA Driver (`dd-shim-cuda`) | 132 | 132 | 0 | Every manifest entry has a body, not that all CUDA semantics/hardware features exist |
| CUDA Runtime (`dd-shim-cudart`) | 49 | 49 | 0 | Runtime manifest has bodies and delegates to the driver layer |

GL's generated unsupported entries now fail truthfully and its partial entries are explicit degraded/spec-no-op
bodies; Vulkan Phase 0 likewise fails unsupported bodies rather than returning success. The remaining risk is
advertising a profile whose mandatory semantics are partial or unsupported. Capability truth must stay generated
from these inventories, with outputs initialized and claims lowered whenever required behavior is absent.

### 2.1 EGL/GLES: functional slice and missing behavior

The hand-written path includes display/config/context lifecycle; window and pbuffer creation; swap; core
buffer, texture, shader/program, uniform, framebuffer, vertex state; clear and draw lowering. Existing unit
tests prove registry census, selected state tracking, C-shim wire parity, clear-frame parity, resource lowering,
and a small number of pixel/translation cases.

It is not a GLES 3.2 implementation. The 106 partial and 78 unsupported entries include major API families visible in the
manifest, including substantial GLES3 functionality: queries, transform feedback, uniform blocks, program
interfaces/pipelines and binaries, samplers, sync objects, multisampling, 3D/compressed textures, image
load/store, compute/indirect dispatch, memory barriers, indexed state, robust getters, and many copy/blit/
invalidate paths. Some hand-written calls also implement only the subset needed by current captures. Therefore
the current library must not claim behavioral GLES 3.2/EGL 1.5 completeness merely because those symbols exist.

Pinned core-profile census (Khronos OpenGL-Registry revision
`9d527dbc81bb76e35ba284fe385ed8a5ddb90cbc`, EGL-Registry revision
`3d7796b3721d93976b6bfe536aa97bbc4bce8667`):

| Runtime claim | Mandatory core calls | Real bodies | Generated stubs | Status |
|---|---:|---:|---:|---|
| GLES 2.0 (default) | 142 | 142 | **0** | command inventory complete; semantic/CTS gaps remain |
| EGL 1.4 | 34 | 34 | **0** | mandatory command inventory complete; semantic/live gates remain |
| GLES 3.0 (`DD_SHIM_ES3=1`) | 246 | 164 | **82** | experimental/partial, must not be presented as conformant |

Commit `dad879b2` closes the mandatory GLES2 command inventory by porting the final 30 state/query/spec-no-op bodies;
the gate is now a normal regression. This proves callable mandatory entry points, not Khronos conformance: semantic
gates below still cover object, texture, draw, readback, synchronization and query correctness. Commit `541e9ad0`
also closes EGL 1.4 at 34/34 with truthful failure semantics for unsupported native-pixmap/client-buffer paths; this
is command/profile coherence, not proof of broad platform integration or EGL CTS conformance.

Specific correctness gaps:

- Feature/version/extension reporting is not generated from a capability table tied to real implementations.
  An application can select a path based on a version or extension whose calls are stubs.
- Commit `592e65bf` adds typed config matching and generation-checked window/pbuffer surfaces. Impossible config
  requests return zero matches, invalid handles/attributes and non-ES API selection report EGL errors, distinct
  surfaces retain dimensions/current draw-read identity, and stale handles fail. Both former red gates now run as
  ordinary Rust regressions. Remaining work is broader per-attribute/config compatibility and native-window/backing
  allocation resize/lifetime coverage, not the former singleton-object contradiction.
- Commit `bf5a1a7f` closes the former singleton-context defect: contexts now have unique typed handles and explicit
  share groups, GL object namespaces follow the current share group, context currentness/errors are thread-local,
  and release/destroy/invalid-handle behavior is covered by normal Rust tests. The formerly ignored gate
  `egl_contexts_are_distinct_shareable_and_current_per_thread` now runs green. Commit `592e65bf` subsequently adds
  typed EGLSurface identity and current draw/read tracking as another normal green regression.
  One narrower validation defect remains: `eglQueryContext` does not check whether its handle is live and succeeds
  after destruction. `egl_query_context_rejects_destroyed_handles_without_mutating_output` preserves that residual
  without keeping the completed context/share-group work falsely red.
- Commit `592e65bf` makes EGL swap transactional at the producer boundary: submission precedes draw-list clearing,
  failure preserves queued work and reports context/surface loss. The former red gate is now a green regression.
  End-to-end presentation still needs typed accepted/executed/generation results, reconnect residency replay, and a
  fault-injected Wayland/executor integration test; the private Wayland commit path remains false-success separately.
  Commit `7bd62343` makes `ExecConn::submit` reject the host's failure acknowledgement byte and preserves the real
  socket-framing regression test as an ordinary green gate. The acknowledgement remains a one-byte protocol; version
  it before adding generation, completion serial or structured error fields.
- Commit `592e65bf` gives `glFlush` a nonblocking submission serial and makes `glFinish` wait for its completion;
  the former no-op sentinel is now green. Executor loss/error propagation and real asynchronous completion remain
  necessary integration coverage rather than reasons to keep the completed symbol-level row red.
- Object and error semantics are incomplete across unported families: lifetime, namespace, validation,
  `glGetError`, query results, synchronization, and cross-context sharing need systematic coverage.
- Even core buffer/texture name semantics are inverted. `glGen*` marks slots as live immediately, so `glIsBuffer`
  and `glIsTexture` return true before first bind; binding a fresh unused name does not create storage; deletion does
  not detach buffers/textures from context/VAO/texture-unit bindings; and several negative counts return silently
  instead of `GL_INVALID_VALUE`. Separate reserved names from instantiated objects in a per-share-group namespace.
  Binding performs typed lazy creation, deletion removes the name and detaches every binding in the deleting context
  while retaining storage referenced by queued draws, and reuse gets a new generation so stale snapshots cannot
  alias it. Generate/delete must validate fully before touching outputs/state.
  `gles_generated_names_binding_and_deletion_follow_object_lifetimes` pins the observable lifecycle.
- Shader/program ownership is also not GLES-compatible. Programs store at most one vertex and fragment id,
  `glDetachShader` is empty, shader deletion immediately erases attached source, and program deletion ignores current
  use. There is no delete-pending state, attachment enumeration/count, or deletion-status query. Store a set of strong
  shader references on each program; flag shaders/programs for deletion; reclaim only after their last attachment or
  current-context reference disappears; implement attach/detach duplicate/error rules and all corresponding getters.
  Linked executables must own an immutable snapshot so later source deletion/recompile cannot mutate them.
  `gles_shader_program_attachment_detach_and_delete_pending_are_consistent` fixes the minimum ownership model.
- Pixel-store/upload validation is unsafe and non-atomic. `glPixelStorei` accepts invalid alignment and negative row/
  skip values; upload byte counts and padded strides use unchecked `usize` arithmetic; `glTexImage2D` ignores target,
  internal format, type and border; subimages do not reject negative/out-of-bounds rectangles; and immutable storage
  records neither format nor mip levels, so it can be redefined. Define a format/type table with bytes/block, allowed
  internal/external pairs and conversion function. Validate all enums/dimensions/levels and compute source layout via
  checked add/multiply/align before allocating, reading client memory or mutating the texture. Store every mip/layer
  plus immutable flag/level count and enforce completeness/filter rules. `gles_pixel_store_and_texture_upload_validation_is_atomic_and_checked`
  pins invalid pixel store, safe arithmetic and immutable state.
- User framebuffers now report missing and incomplete color attachments, accept only live typed texture/renderbuffer
  names, and block clear/draw atomically with `GL_INVALID_FRAMEBUFFER_OPERATION`. The default framebuffer remains
  complete. This closes the unconditional-complete and silent-draw contradiction for the implemented color-only
  path. Depth/stencil attachments, dimension/sample matching across multiple attachments, layered/cube targets,
  generation-tagged attachment references, `GL_FRAMEBUFFER_UNSUPPORTED`, and completeness guards on read/blit remain.
- Core array/index draws now run one side-effect-free validator before snapshot or recording. It validates primitive
  mode, negative first/count, index type/alignment/range, linked current program, color-FBO completeness, and every
  enabled attribute's type/shape/stride/source range with checked arithmetic. Invalid draws report the appropriate GL
  error without replacing the pending first error. Remaining work is mapped-buffer state, backend-derived limits,
  restart/base-vertex forms, and preserving rather than collapsing instance/base-instance counts in IR.
- `glReadPixels` constructs and zero-fills the caller slice before validating type or framebuffer, so an invalid call
  corrupts output while reporting no error. It ignores all pack state and uses unchecked width×height×bpp arithmetic;
  default-framebuffer readback is not a synchronized backend read. Validate format/type/dimensions/FBO and calculate
  checked pack row/skip layout before touching output. Synchronize the producing serial, read the selected attachment,
  convert/clamp according to its format, and write only addressed pixels/rows while respecting pack alignment,
  row length and skips. `gles_readpixels_validates_pack_layout_and_preserves_output_on_error` fixes non-mutation and
  the missing pack vocabulary before pixel parity is trusted.
- GLES sync objects are symbol/default-only. Fence creation and waits are unsupported generated stubs while
  delete/is/query return harmless sentinels; no sync record captures a command-stream point. The shared IR already has
  fences and submit signals, and software/wgpu have partial backends, but the default Metal executor lacks the fence
  trait methods and the one-byte socket acknowledgement carries no submission identity. Give each context a monotonic
  submission serial. `glFenceSync` closes/flushes prior commands and records that serial; executor responses publish
  accepted/completed serials; `glClientWaitSync` implements zero/finite/infinite timeout plus
  `GL_SYNC_FLUSH_COMMANDS_BIT`; server-side `glWaitSync` inserts a dependency without CPU blocking. Preserve sync
  lifetime across pending waits and context sharing, and map device/transport loss to `GL_WAIT_FAILED` plus GL error.
  Implement the same serial contract in Metal, wgpu and software so parity tests can distinguish already-signaled,
  condition-satisfied and timeout. `gles_sync_objects_track_real_submission_completion_and_wait_results` pins every
  layer instead of accepting sentinel returns.
- ES3 query objects are names without objects. Generation increments a counter, begin/end are partial no-ops,
  delete/is/get return defaults, and neither IR nor backend capabilities describe occlusion or primitive-count query
  work. Implement typed query records with target, active owner/context, begin/end submission serials, availability,
  result and delete-pending state. Enforce one active query per target, non-nesting and matching end; result-available
  must never block, while result retrieval waits only for its resolve serial. Add IR begin/end/resolve operations and
  backend capability bits; implement Metal visibility/counter buffers and wgpu query sets, with software deterministic
  fixtures. Timer/disjoint extensions must remain unadvertised until timestamp period, monotonicity, wrap and GPU
  disjoint/reset behavior are real. `gles_query_objects_track_targets_availability_and_asynchronous_results` pins
  lifecycle, observable readiness and negotiation rather than fabricated zero results.
- Shader handling has two contracts. Vulkan forwards SPIR-V, while captured GL work can carry MSL-shaped bytes;
  wgpu then substitutes narrow built-in WGSL shaders when naga cannot consume that payload. This is workload
  pattern matching, not general shader correctness.
- Commit `592e65bf` makes compile/link status and logs truthful for malformed source and missing compiled stage pairs;
  that former red gate is now green. Its dependency-free validator is not full GLSL ES: production completeness still
  requires a real parser/compiler, interface and precision validation, translation artifacts, and a negative shader
  corpus covering version/profile, overload, uniform, varying and resource-limit failures.
- Orientation is heuristic. Both executor implementations contain offscreen-target flip logic based on target
  identity/history. Existing rendering notes record captures for which a static offscreen flip corrected one
  stream and inverted another. Orientation must be explicit in IR or derived from actual viewport/projection
  state, never guessed from “surface vs texture”.
  The synthesized Metal golden baselines are now tracked outside ignored `target-*` output, and the asymmetric
  orientation fixture pins the GL-coordinate quadrant contract with direct pixel assertions before PNG comparison.
  Current Metal and wgpu replay match all four synthesized cases bit-exactly. This proves current-backend parity for
  those fixtures; it does not close the data-driven-orientation gap or replace a real captured-stream oracle.
- The tests are mostly headless state/wire tests. They do not run a Khronos conformance suite, broad GLES3
  samples, context-sharing races, resize/recreate loops, or long-running Chrome GPU workloads.

### 2.2 Vulkan: bring-up implementation, not a general ICD

The 94 bodies cover loader negotiation, one physical device/queue, basic memory and resources, shader and
graphics/compute pipeline creation, descriptors, a small command set, simple fence/semaphore handling, and a
Wayland FIFO swapchain. That is enough to target compute/triangle/vkcube-style validation, not general Vulkan.

Important gaps and incorrect simplifications:

- **599 commands return generated defaults.** This includes most modern synchronization, dynamic rendering,
  barriers/events, queries/timestamps, sparse resources, secondary command-buffer behavior, transfer variants,
  descriptor extensions, and nearly all extension functionality.
- Extension and feature enumeration must be the allow-list of semantics actually supported. Resolving every
  registry command while advertising a narrower coherent device is preferable to falsely advertising support.
- Swapchain acquisition now owns only available images and atomically signals valid guest-side acquire fences or
  semaphores. Exhaustion distinguishes zero-timeout `VK_NOT_READY` from finite `VK_TIMEOUT`, presentation requires
  prior acquisition and synchronously releases ownership, replacement retires the old swapchain for new acquisition
  while allowing its already-acquired images to finish, and surface destruction marks dependent swapchains lost.
  This is still the synchronous transport model: it does not prove asynchronous display-engine completion, present
  wait-semaphore ordering, resize generations or `SUBOPTIMAL` behavior.
- Queue/fence/semaphore handling is a bring-up model, not a spec-grade dependency graph. Binary and timeline
  semaphore semantics, stage masks, multiple queues, and host visibility rules need real state transitions.
- WSI reports fixed capabilities (one format, FIFO, identity transform, opaque alpha). Resize, surface loss,
  current extent, swapchain replacement (`oldSwapchain`), present result arrays, and multi-window lifetimes need
  validation.
- WSI surface/create validation now uses one capability profile for queries and swapchain creation. Surface queries
  reject stale handles without mutating outputs, queue-family support is truthful, duplicate Wayland native windows
  fail, and swapchain creation validates image count, extent, format/color space, usage, layers, sharing mode,
  transform, composite alpha, present mode and live same-surface `oldSwapchain` before allocating. The behavioral
  Rust ABI regression `wsi_validates_surface_handles_and_swapchain_create_info_atomically` pins the negative matrix
  and output/state atomicity. Resize generations, surface loss propagation into existing swapchains and actual
  `oldSwapchain` retirement remain part of the lifecycle row below.
- Swapchain images implement `Available -> Acquired -> Presenting -> Available`, and swapchains implement
  `Active/Retired/Lost`. The Rust ABI regression
  `swapchain_tracks_image_ownership_timeouts_and_retirement` covers two images in flight, exhausted zero/finite
  timeouts with preserved outputs and unsignaled fences, acquire-before-present, synchronous release/reacquire,
  replacement retirement, completion of an old acquired image and surface loss. True asynchronous present-engine
  completion and resize generations remain outside this synchronous lifecycle slice.
- Queue present now validates every wait semaphore, swapchain, image index and acquired ownership before delivery;
  invalid batches do not mutate state. Executor rejection/loss maps to `VK_ERROR_DEVICE_LOST`, Wayland commit failure
  maps to `VK_ERROR_SURFACE_LOST_KHR`, and every path populates `pResults`. Wait semaphores, the global IR cursor and
  image ownership commit only after the complete synchronous delivery batch succeeds, so a failed single-swapchain
  frame can be retried unchanged. The Rust ABI fault regression
  `present_failures_preserve_ir_ownership_and_waits_until_transactional_commit` covers executor and surface faults,
  retry, result arrays and invalid-wait atomicity. The current one-byte executor acknowledgement cannot roll back a
  frame already accepted before a later surface in a multi-swapchain batch fails; a versioned batch protocol is still
  required for external all-or-nothing delivery. Resize generations and `VK_SUBOPTIMAL_KHR` also remain unmodeled.
- Memory types, coherence, alignment, image layout transitions, subresources, mip levels, array layers,
  multisampling, and format capabilities are far narrower than Vulkan's model. Returning success where the IR or
  backend cannot represent the operation is wrong.
- Commit `be61c109`, ported from MoltenVK's ownership model, closes the foundational memory safety slice: allocation
  is fallible/type-checked, buffer/image binds validate identity/alignment/range/rebind, and mapping validates ranges,
  host visibility and double maps. Its Rust gate is now green. Remaining Vulkan memory work is nontrivial: multiple
  heaps/types and property truth, coherent/noncoherent flush/invalidate, aliasing, sparse/external memory, and keeping
  bound storage alive through real asynchronous submissions.
- The same commit adds `Initial/Recording/Executable/Pending/Invalid` command-buffer state, pool ownership, usage
  flags, atomic submit validation, guest-side binary semaphore/fence transitions and idle retirement; that former
  ledger gate is now green. Residual work includes pool reset flags/generations, secondary command buffers, events,
  timeline semaphores, true asynchronous completion and device-loss propagation rather than synchronous retirement.
- Commit `4c97adf5` ports MoltenVK-derived descriptor identity/validation: immutable layouts, pool capacity and
  ownership, typed arrays, atomic writes/copies, compatible pipeline-layout binding and dynamic offsets. Its former
  red gate is now a normal regression. Residual descriptor work includes update-after-bind/variable counts, inline
  uniform blocks, texel buffers, immutable samplers and lifetime retention through truly asynchronous submission.
- The same commit closes default-substitution in the implemented pipeline/render-pass/framebuffer slice: records keep
  normalized supported fixed state and attachment/subpass compatibility, invalid references/state fail, and begin
  render pass no longer fabricates missing objects. The compatibility gate is green. Remaining breadth includes full
  SPIR-V interface reflection, multiple subpasses/dependencies, input/resolve attachments, multiview, derivative/
  library pipelines, dynamic rendering and every unsupported fixed-function state—each must reject until implemented.
- Images retain only width, height, format and a render-target bit. Initial/current layout, aspects, mip/layer count,
  samples, tiling, usage, queue ownership and bound memory are absent. Both legacy and synchronization2 pipeline
  barriers are generated no-ops, so access masks and ownership transfers never constrain execution. Introduce a
  per-subresource state table keyed by aspect/mip/layer with layout, last access/stage, owner queue and initialization;
  record validated transition intents in command buffers, then apply them in submission order. Reject old-layout or
  ownership mismatches, enforce legal layout/usage combinations, and lower hazards to the backend's explicit barrier
  model (or ordered encoder/pass boundaries where Metal supplies the guarantee). Keep swapchain present transitions
  in the same model. `vk_image_layout_barriers_track_subresources_and_queue_ownership` pins the state and both APIs.
- Commit `aba9a0f4` lands a meaningful Phase-3 shared-IR/backend slice: `TextureSubresource` (mip/layer/aspect),
  `Origin3d`, `Extent3d`, exact texture-to-texture copy and filtered blit, with software, wgpu and Metal validation
  tests. Do not remove that implemented progress from the audit. The Vulkan shim still exposes only buffer-to-buffer
  copy: buffer/image copies, image copies, blits, resolves and image clears remain generated failures/no-ops within
  Vulkan 1.0. IR also still lacks multi-layer region count, Vulkan buffer row-length/image-height semantics, explicit
  layouts and multisample resolve. Extend the landed types rather than replacing them; add buffer/texture region
  structs and a distinct resolve command, then lower every Vulkan region. Validate with checked block/row arithmetic,
  format/sample compatibility, overlap rules and layout state before mutation.
  `vk_transfer_commands_preserve_every_region_subresource_and_layout` now guards both the landed slice and residuals.
- Shader module creation performs no SPIR-V validation: `codeSize` is truncated to words, null code becomes an empty
  successful module, and arbitrary bytes are recorded before any structural check. Pipeline creation substitutes
  entry name `main`, ignores specialization constants, and performs no reflection/interface validation for stages,
  descriptors, push constants, vertex inputs or fragment outputs. Parse and validate SPIR-V into an immutable module
  record at creation (checked header/version/bounds/instruction stream); reflect entry points and interfaces; apply
  specialization constants to a pipeline-local module; then validate stage linkage and pipeline-layout/render-target
  compatibility before emitting IR. Preserve exact diagnostics internally and return a pipeline-creation error with
  the failed output handle left null. `vk_shader_modules_validate_spirv_entries_specialization_and_interfaces` pins
  validation rather than mere byte forwarding.
- The shared shader payload has no provenance. Both host graphics backends deliberately treat translation failure or
  missing modules as permission to select built-in flat/textured shaders: wgpu caches naga failures and returns
  `Ok(())`, while bespoke Metal cannot consume real SPIR-V and substitutes its built-in library. This makes a Vulkan
  pipeline render plausible but wrong pixels. Replace `CreateShader { spirv }` with a tagged payload/contract such as
  strict `SpirV`, transitional `LegacyMsl`, `Wgsl` and `PtxKernel`; builtin selection must be an explicit test/demo
  pipeline kind, never an implicit failure path. Shader creation/pipeline linking must return a backend error that
  propagates through the typed executor acknowledgement to Vulkan. Add negative shaders whose output differs visibly
  from every builtin. `vulkan_shader_translation_failure_never_falls_back_to_builtin_rendering` enforces both backends.
- Vulkan advertises `robustBufferAccess = VK_TRUE`, but descriptor ranges, vertex/index fetches and many resource
  bounds are not validated, and the backends have no shared zero-out-of-bounds-read/discard-write policy. Set the
  feature false immediately. Re-enable it only after every shader-visible buffer path is either instrumented during
  translation or uses backend-native robustness with identical semantics, including dynamic offsets and partially
  bound ranges. Use adversarial shaders and guard regions to prove zero reads, discarded writes and no neighboring
  resource corruption on all executors. `vk_robust_buffer_access_is_advertised_only_with_zeroing_and_bounds_guarantees`
  prevents a capability bit from standing in for the guarantee.
- The checked-in C smoke file is not a loader-level conformance or live-present suite. The Rust Vulkan crate has
  only three unit tests: census/dispatch and a synthetic IR seam round-trip.

### 2.3 CUDA Driver and Runtime: symbol-complete, still capability-limited

These two manifests currently have real bodies and zero generated stubs. This is stronger than GL/Vulkan, but
does not make dd a complete CUDA device. The implementation maps a selected CUDA model through PTX and the shared
IR. Unsupported PTX instructions, CUDA memory modes, streams/events ordering, modules/linking, textures/surfaces,
graphs, IPC, peer access, unified memory, device attributes, and numerical edge cases must either work or return
accurate CUDA errors. Tests should state which CUDA version and workload classes are supported; symbol count is
not the acceptance criterion.

The former unsupported-PTX false-success path is closed by `de599028`: driver launch now returns
`CUDA_ERROR_NOT_SUPPORTED` or `CUDA_ERROR_INVALID_PTX`, the Runtime maps the same failure instead of returning
`cudaSuccess`, and both record diagnostics through the capability/stub tracker. The Rust gates
`cuda_driver_rejects_unsupported_ptx_instead_of_false_success` and
`cuda_runtime_source_does_not_convert_ptx_compile_failure_to_success` now run normally. Remaining CUDA work is
capability breadth and execution correctness, not this retired false-success finding.

## 3. Shared IR and executor gaps

The IR has a useful, testable core: resource creation/destruction, buffer writes, textures/samplers/shaders,
render and compute pipelines, bind groups, surfaces/fences, submit/wait/present, render/compute pass boundaries,
draw/dispatch, and three copy directions. Encoding/decoding and the CPU/mock backends provide good headless
validation.

Executor recovery currently has a disconnected contract. `ExecConn` correctly detects a reconnect and exposes
`take_residency_reset`, because a new per-connection backend has empty buffers, textures, shaders, pipelines and bind
groups. No GL or Vulkan producer calls it. Vulkan then sends only IR after `present_flushed`, so the replacement
executor sees references to nonexistent resources; GL may happen to re-emit some frame-local snapshots but has no
complete generation proof. Add an executor generation to typed acknowledgements and every resident object. On a new
generation, transactionally replay dependency-ordered creation/content/state before the next submission, or mark the
GLES context/Vulkan device lost when reconstruction is impossible. Once lost, reject all relevant later calls with
`GL_CONTEXT_LOST`/`EGL_CONTEXT_LOST` or `VK_ERROR_DEVICE_LOST`, preserve destroy/query safety, and never present stale
pixels as recovery. Fault-injection must kill/restart the executor between upload, draw, ack and present and compare
the recovered frame plus bounded cache/fd growth. `executor_reconnect_replays_complete_residency_or_reports_api_loss`
pins both producers and their API loss states.

It is not expressive enough for the API versions exported by the shims. Missing concepts include explicit
barriers and resource states, queue ownership, events/queries/timestamps, indirect execution, push constants,
dynamic offsets with complete validation, texture-to-texture copy/blit/resolve, mip/layer/aspect ranges, texture
views as first-class resources, storage texture access details, multisample resolve, stencil operations,
transform feedback, and a precise external-memory/fence contract.

The bespoke Metal executor has an explicit `GpuError::Unsupported("metal replay: command not implemented")`
fallback in its encoder replay. Its validation test currently expects dispatch in the render executor to be
unsupported. That directly conflicts with a general compute/CUDA/Vulkan claim: compute needs a deliberate Metal
implementation or must be routed only to a backend that supports it.

The wgpu executor implements a broader shared-IR path, but its compatibility fallback for non-SPIR-V GL shader
payloads chooses built-in flat/textured WGSL programs. This can reproduce selected golden captures while silently
misrendering arbitrary shaders. It also emulates fences with submission completion rather than an external
timeline primitive. Device-level IOSurface examples are not ordinary CI tests, so unit-green does not prove
zero-copy presentation on a real Metal device.

Commit `a8ccefaa` closes both capability contradictions. The wgpu backend no longer advertises unsupported
IOSurface presentation, and the shared protocol now serializes a versioned descriptor covering wire/command bits,
shader payloads, texture formats, limits, present kinds and timeline fences; incompatible peers fail with typed
`Unsupported` before emitting an unknown command. Both former ignored gates are ordinary green regressions.
Remaining work is to derive every GL/Vulkan/CUDA public claim from the negotiated intersection and to version any
future field/tag evolution without silently defaulting old peers into broader behavior.

Central Rust coverage now exists in `dd-tests/tests/rendering_ir.rs`: it constructs all 21 `Cmd` variants and all
19 `Enc` variants with non-default descriptors, bind-resource kinds, optional fields and signed/64-bit values;
round-trips the entire corpus; rejects every truncated prefix of every standalone command; rejects unknown command
and encoder tags; and verifies duplicate ids, use-after-destroy and out-of-bounds behavior through the software
backend. Exhaustive Rust matches intentionally make a new enum variant fail compilation until its corpus case is
added. This proves serialization and selected lifecycle validation, not executor implementation or pixel results.

Adversarial coverage additionally forges `u32::MAX` shader-word, string, bind-entry, encoder-op, render-attachment
and zero-copy `WriteBuffer` lengths; checks canonical booleans; and proves the fast path validates the complete
payload before calling the backend. Commit `7bd62343` closes two trust-boundary gaps: replay now validates a complete
frame before applying any command while retaining borrowed `WriteBuffer` payloads, and render-state float decoding
rejects NaN and infinity. The green Rust gates preserve both guarantees. Semantic range validation remains: require
nonnegative bounded viewport sizes, an ordered API-valid depth range, and checked rectangle/copy arithmetic.

Commit `a8ccefaa` adds the versioned capability preamble and the socket reader caps a frame at the advertised 64 MiB,
but the remaining negotiated limits are descriptive. Replay receives no `Capabilities`; `CreateBuffer` can exceed
`max_buffer_bytes`, textures can exceed dimensions/byte footprint, and bind-group limits are not centrally checked
before software/Metal/wgpu allocate. Introduce `ReplayLimits` derived from the negotiated intersection and validate
the entire already-decoded frame against it during the atomic prepass. Use shared checked descriptor validators for
all backends and a typed `ResourceLimit` error; use fallible reservations before host allocation and leave state
unchanged on failure. Test exact limit, limit+1, arithmetic overflow, huge mip/sample footprints, and ensure a rejected
frame produces NACK without backend mutation. `executor_enforces_every_negotiated_limit_before_decoding_or_allocating`
pins that negotiation must control execution rather than merely document it.

Per-object maxima are insufficient against many legal objects. Each executor connection needs an
`ExecutorResourceBudget` charging buffer/texture bytes (including mips/layers/samples/alignment), shader source/
compiled artifacts, pipelines/bind groups and object counts before creation, with exact refund on destroy and whole-
generation teardown on disconnect. Put a global process/device budget above connection budgets so many clients cannot
collectively exhaust Metal/wgpu. Cache sharing must charge retained ownership consistently and eviction must respect
in-flight references. Add create-to-limit/destroy/recreate and abrupt-disconnect Rust tests, plus concurrent clients
where one is rejected while another continues rendering. `executor_accounts_cumulative_residency_and_object_counts_per_connection`
pins aggregate residency separately from compositor-side surface/shm quotas.

## 4. Compositor and presentation gaps

- Smithay executor startup is now implemented in `dd-compositor/src/gpu.rs` (commit `b2d5af7b`) before compositor
  mode selection, with backend selection and a dmabuf-import health warning. It remains unverified live. The
  running flag is set before the executor thread binds its socket, so the health check proves “spawn requested”,
  not listener readiness; replace it with a bind/ready result channel and fail accelerated startup on bind error.
- `zwp_linux_dmabuf` feedback is opt-in because a live GTK4 run exposed failure mapping Smithay's format-table
  fd into the guest. The host-side short-name/sealed-file patch does not by itself prove the cross-OS fd/mmap
  contract. The format table must use a guest-mappable transport object, and Linux `dev_t` serialization must be
  byte-correct rather than relying on the macOS host type width.
- Smithay popup-as-native-window behavior is gated pending live menu validation. Default compositing clips popup
  content to its parent; this is visibly incomplete for menus extending beyond the toplevel.
- Live evidence is uneven: software GTK on Smithay was exercised through `PngPresenter`, but Chrome, the native
  Cocoa window/input loop, accelerated dmabuf, cursor/IME/clipboard, popups, resize, and multi-window operation
  were not jointly exercised on that path.
- Historical rendering notes identify multi-process Chrome content as blank above the proven low-level fd,
  futex, eventfd, epoll and bootstrap-handle primitives. This remains an end-user rendering blocker even if the
  failure is Chrome/Mojo process bring-up rather than rasterization or Metal compositing.

`dd-tests/tests/rendering_backends.rs` now keeps the landed Smithay wiring green: startup precedes both modes,
honors backend selection, calls the shared executor, and checks accelerated imports. Replace this source gate with
a behavioral socket-ready/capability handshake and live dmabuf render once the executor reports readiness.

## 5. What “surface complete” must mean

Use three explicit labels for every front end:

- **ABI complete:** all symbols required for the declared API are exported with correct calling conventions.
- **Advertised-feature complete:** every advertised version, extension, format, limit, and feature has correct
  behavior, including errors and synchronization.
- **Workload validated:** named unmodified applications pass repeatable live and image/compute correctness gates.

Only the first label currently applies broadly to GL and Vulkan. A stub must be represented as `stub`, not
“implemented”. A partial body must be represented as `partial` with its supported parameter domain. Documentation
and runtime debug output should use the same generated capability inventory.

### 5.1 Definition of done for arbitrary GUI applications

“Any GUI app” cannot honestly mean every historical or proprietary Linux graphics stack without defining the
platform boundary. The defensible product target is: an unmodified **Wayland-native Linux desktop application**
using software buffers, EGL/OpenGL ES, or the advertised Vulkan profile behaves like it does on a conformant
desktop compositor. X11-only applications require XWayland (or a separate X11 server) and desktop OpenGL-only
applications require a GLX/OpenGL compatibility strategy; neither surface exists in the audited rendering path.
Those are release-blocking omissions if the product promise literally includes arbitrary Linux GUI binaries.

Full GUI compatibility requires all of these layers simultaneously:

| Area | Required for “any GUI app” | Current shortfall / review gate |
|---|---|---|
| Display transport | multi-client Wayland socket, fd passing, shared memory, disconnect isolation | Primitive gates exist, but long-lived multi-process Chrome still exposes a higher-level IPC failure; stress and resource exhaustion are not broadly gated |
| Software rendering | `wl_shm` ARGB/XRGB, pools, resize, damage, scale/transform, release ordering | Strongest path today; needs format expansion where toolkits require it, overflow/seal/SIGBUS hardening, multi-client lifetime and conformance tests |
| Window management | xdg toplevels/popups, configure/ack, states, move/resize, activation, decorations, parent/child and transient rules | Broad Smithay coverage, but native popup path is gated; live menus, modal/transient stacking, resize storms, minimize/fullscreen and multi-window focus are not proven together |
| Surface composition | synchronized/desynchronized subsurfaces, nested trees, viewport crop/scale, fractional scale, buffer transforms, regions, frame callbacks | Implemented slices exist; arbitrary nesting, reparent/destruction races, transform combinations, occlusion and callback pacing need protocol and pixel tests |
| GPU APIs | truthful EGL/GLES and Vulkan implementations with no success stubs in advertised profiles | GL has 236 stubs; Vulkan has 599; desktop OpenGL/GLX is absent; shader and synchronization contracts are incomplete |
| GPU presentation | dmabuf feedback/import, IOSurface identity, explicit synchronization, resize/recreate, multiple frames in flight | Smithay executor startup and opt-in guest-mappable feedback are implemented but live GPU presentation remains unverified; stable allocation identity, explicit sync and robust swapchain lifecycle are missing |
| Input | complete keymap, modifiers/repeat, pointer buttons/axis/gestures, cursor surfaces/shapes, grabs/constraints, relative motion, touch/tablet | Keyboard/pointer and several extensions exist; key translation is explicitly a ported subset, touch has no real device, tablet/gesture protocols and broad shortcut/dead-key/layout testing are absent |
| Text/IME | text-input v3 focus, preedit/commit/delete-surrounding, content type, cursor rectangles, host IME | Protocol implementation exists; multilingual composition, candidate UI positioning, surrounding-text edits, focus transfer and toolkit matrix need live proof |
| Clipboard/DnD | data device, primary selection, MIME negotiation, streaming, drag icons/actions, host bridge | Text clipboard paths exist; arbitrary MIME/binary payloads, large transfers, cancellation, DnD actions/icons and sandboxed producer death need completion |
| Outputs/color | multiple outputs, integer/fractional scale, hotplug, transforms, refresh/presentation timing, ICC/HDR/color spaces | Multi-output protocol readiness is tested, but native multi-display movement/hotplug is not; color management/HDR/wide gamut is not represented end-to-end |
| Accessibility/desktop integration | accessibility bus, notifications, portals/file chooser, URI launching, app identity | Outside the current compositor/shim implementation but required by many “full” desktop apps; inventory and route through host services are missing |
| Compatibility | XWayland for X11/GLX apps; possibly OpenGL 3.x/4.x compatibility beyond GLES; toolkit-specific expectations | No audited XWayland/X11/GLX path. Qt, SDL, GLFW, Electron, Java/AWT, Wine and game engines do not have an explicit test/support matrix |
| Robustness | protocol errors, malicious sizes/counts, GPU reset/device loss, app crash, compositor restart, bounded memory and fd use | Some validation/robustness tests exist; there is no whole-stack fault-injection, device-loss recovery, leak soak or per-client resource quota gate |

Commit `541e9ad0` replaces the EGL shim's scripted Wayland exchange with a decoded handshake: globals are discovered
by interface/name and clamped version, the received configure serial is acknowledged, ping is answered, and display
errors are decoded. Its transport now performs fallible full writes/fd passing and returns typed disconnect, protocol
and frame-timeout failures through transactional swap. Both former red gates are normal regressions. Remaining work
is reconnect/session regeneration, explicit synchronization, fd-ownership fuzzing, and live interoperability against
non-dd compositors rather than convenient in-process object/event ordering.

Commit `a8845499` replaces boolean pacing with structured outcomes, retains bounded frame callbacks across failed
presentation, propagates presenter output errors, and rejects accelerated imports without a healthy executor and a
valid IOSurface description. Those three former red gates now run green. A live macOS Metal run is still required to
verify device-loss and drawable timing; source/unit evidence does not prove scanout completion.

Commit `06e002e7` closes the source-level mixed-tree omission: GPU/IOSurface roots now include shm/IOSurface
subsurfaces and popups through a common composition representation, with ordinary regression coverage. Remaining
proof is live Metal pixel parity for mixed roots, deep synchronized/desynchronized nesting, clipping/occlusion and
failure/release ordering; those integration requirements remain in the matrix rather than keeping landed code red.

Buffer lifetime is now explicit rather than inherited from Smithay's release-on-next-attach policy. Each attachment
creates a generation-tagged `BufferUse`: shm emits exactly one release only after its validated pixels are copied into
the owned cache, including same-proxy reuse; zero-copy buffers remain retained across failed/offscreen presentation
and accepted delivery completes the active use exactly once. Replacement, detach and surface teardown retire the
active generation without double release. The remaining gap is real GPU completion: Metal currently reports
`Delivered` after scheduling `presentDrawable`, and the Presenter ABI exposes no completion fence/poll token. Failed
zero-copy generations are therefore retained rather than prematurely released, but cannot yet be reclaimed in serial
order. Extend `PresentOutcome` with a completion token plus polling/callback delivery, then test triple buffering,
destroyed proxies and out-of-order completion. `compositor_releases_buffers_only_after_the_last_cpu_or_gpu_use`
remains partial until that device evidence exists.

Presentation failure classification is still internally contradictory. `FramePacing::Failed` retains frame
callbacks so the same dirty frame can retry, but drains and discards that commit's one-shot `wp_presentation`
feedback immediately. If a retry later delivers the frame, the client can never receive its actual timestamp; if
the failure was terminal, retaining callbacks is equally wrong. Give `PresentError` an explicit retry class (or use
separate `RetryableFailure`/`TerminalFailure` outcomes). A retryable failure retains dirty state, buffers, frame
callbacks and presentation-feedback objects together; eventual delivery completes all of them with one serial and
timestamp. A terminal failure discards feedback, destroys callbacks without `done`, releases resources, and reports
the surface/device failure upstream. Bound retained objects per surface and expire them only through an explicit
terminal transition, not an arbitrary timeout that fabricates success. Test fail-fail-deliver and terminal-fail
sequences with multiple feedback objects and confirm exact-once protocol destruction.
`compositor_retains_presentation_feedback_across_retryable_failure_only` pins the split.

Multi-output advertisement currently exceeds the compositor's operational model. `add_output` creates additional
`wl_output`/xdg-output globals and logical geometry, but no surface-output membership exists and no code emits
`wl_surface.enter`/`leave`. Fractional scale always reads primary `self.output`; `xdg_toplevel.set_fullscreen(output)`
discards its requested output; window placement and presentation carry no output identity. Toolkits therefore see
monitors they can enumerate but cannot reliably place, fullscreen, scale, or color/present a surface on. Introduce
`SurfaceOutputState { entered: set<OutputId>, primary: OutputId }`, update it atomically when host-window geometry
crosses output rectangles, and emit exact enter/leave deltas. Choose primary by greatest intersection (then stable
nearest/tie-break), derive integer/fractional scale and refresh from it, send preferred-scale/configure before asking
for a replacement buffer, and carry `target_output` through `SurfaceBuffer`/Presenter. Honour an authorized live
fullscreen output and fall back deterministically if it disappears. A Rust two-output geometry state-machine test
plus a C Wayland guest must verify event ordering, scale changes, requested fullscreen and independent windows.
`compositor_routes_each_surface_through_its_actual_output_membership` pins the required coupling.

Output hotplug is also one-way: extras can be added and retained forever but never removed, their globals cannot be
retired, and attached surfaces cannot migrate. Add stable `OutputId` records separate from Smithay handles; process
host display changes as one transaction that updates geometry/modes, migrates affected surfaces, emits leave/enter,
recomputes scale/fullscreen, sends configure, and only then destroys the old global after queued events are flushed.
Never strand focus, popup placement or a native presenter target on a removed output. Exercise add→move→fullscreen→
remove while frames are pending, including removal of the primary and final output; the final-output policy must be
explicit (headless/offscreen or terminal display loss). `compositor_output_hotplug_migrates_surfaces_and_reconfigures_scale`
prevents static enumeration from being mistaken for hotplug support.

Color is not represented end to end. Shared IR distinguishes `*Unorm` from `*Srgb`, and Metal/wgpu choose native
sRGB formats, but the software backend filters and blends the encoded 8-bit channels directly. Correct sRGB
sampling decodes texels to linear light before interpolation; render-target blending decodes destination RGB,
performs blend equations in linear space, and encodes RGB afterward while alpha remains linear. Implement shared,
table-tested IEC 61966-2-1 transfer helpers (including exact breakpoint/rounding), select them from the texture and
attachment formats, and add golden mid-gray interpolation plus translucent source-over cases that visibly differ
from byte-space math. Copies remain bit-preserving unless an API conversion operation explicitly requests color
conversion. `software_backend_applies_srgb_transfer_functions_around_filtering_and_blending` prevents format enums
from masquerading as color-correct software execution.

The compositor/presenter loses broader color intent completely: `SurfaceBuffer` has pixels and an opaque/XRGB flag,
but no primaries, transfer function, matrix/range, mastering luminance or target output profile; Cocoa bitmap paths
use `NSDeviceRGBColorSpace`. Compose the vendored color-management protocol if available (otherwise vendor/pin the
protocol explicitly), store immutable validated `ColorDescription` objects per committed surface, and attach the
description to each buffer use. Each output needs a stable `OutputColorProfile` derived from its ICC/ColorSync data
and HDR capabilities. The composition pass converts every source into a chosen linear working space, blends there,
then converts/tone-maps once into the destination; do not apply sRGB conversion twice when a GPU texture is already
typed sRGB. Unsupported HDR/wide-gamut claims must be omitted, with deterministic SDR fallback rather than clipping.
Test tagged sRGB, Display-P3 and PQ fixtures on SDR and HDR targets, profile hotplug, screenshots, mixed-color-space
subsurfaces and alpha edges. `compositor_negotiates_surface_color_and_converts_to_the_target_output_profile` pins
the protocol-to-presenter metadata path.

Wayland region state is decoded by Smithay but effectively absent from dd's policy. Host events are routed by native
window/surface id and rectangular coordinates without testing the committed `wl_surface.set_input_region`; holes and
shaped child surfaces therefore intercept clicks they must pass through. `set_opaque_region` is likewise not carried
into the tree snapshot, so the compositor cannot safely cull covered nodes or distinguish proven-opaque pixels from
alpha content. Build one region algebra over normalized non-overlapping rectangles with checked union/subtract/
intersection. Map regions from surface logical coordinates through viewport destination, scale, all buffer transforms
and parent/popup offsets. Hit-test the scene front-to-back against input regions (null means infinite, empty means no
input); use opaque regions only for occlusion/damage optimization, never to change pixels. Test holes, disjoint rects,
negative child offsets, transformed/scaled regions and region destruction after commit.
`compositor_honors_input_and_opaque_regions_through_surface_transforms` pins both consumers.

Visibility is also not modeled. `xdg_toplevel.set_minimized` is an empty handler, so the native window remains shown;
there is no mapped/minimized/occluded state and commits continue expensive presentation as if visible. Add
`SurfaceVisibility { Unmapped, Visible, Occluded, Minimized }` owned by the toplevel/native window. Presenter hooks
must hide/minimize and report host occlusion/reveal. Hidden commits update retained content and union bounded damage
but do not claim `wp_presentation.presented`; frame callbacks may be withheld or deliberately throttled according to
one documented policy, never emitted at full refresh as fabricated visibility. Reveal forces one full correct frame,
then completes eligible callbacks/feedback with its real delivery. Popups and child visibility inherit the root;
focus/input/idle-inhibit must leave minimized roots. Test minimize→many commits→restore, host occlusion, popup teardown,
buffer release and bounded callback/damage storage. `compositor_minimize_and_occlusion_control_native_visibility_and_frame_pacing`
prevents protocol acknowledgement from being confused with window-manager behavior.

Presentation feedback has a richer backend contract but discards it at the compositor boundary. Cocoa/other
presenters return `Delivered { serial, timing }`; `present_render_root` matches only `{ .. }`, then each surface's
feedback path independently increments `present_seq`, samples a new host timestamp, reports primary `self.output`,
hard-codes the advertised 60 Hz interval and asserts `Vsync`. A root plus two children from one composite can thus
receive different MSC values for the same output frame, and variable-refresh/non-vsync/off-output delivery is
fabricated. Construct one immutable `PresentedFrame { output, serial/msc, time, refresh, flags }` when the presenter
delivers. Prefer verified backend hardware timing; when unavailable, mark timing/flags conservatively rather than
claiming scanout properties. Pass the same record through `pace_tree` to all feedback objects participating in that
composite, increment a per-output MSC exactly once, and keep frame-callback milliseconds in the same monotonic domain.
Validate timestamp normalization/overflow and guest clock-domain mapping explicitly. Test a three-surface tree,
two outputs with different refresh, missing timing, variable refresh, skipped and failed frames.
`compositor_presentation_feedback_uses_one_backend_evidence_record_per_output_frame` pins the evidence boundary.

The macOS producer of that evidence is unfinished too. Metal returns `Delivered { timing: None }` after command/
drawable submission; it does not install a drawable presented handler or read `presentedTime`, so “delivered” is
not proven scanout and compositor fallback time can precede visibility by a queue interval. Give every submitted
drawable a stable presentation serial, retain its frame evidence until `CAMetalDrawable` completion, convert the
host presentation timestamp into the declared monotonic domain, and obtain refresh from the target screen/display
link (including display moves and variable refresh). Device loss, drawable timeout and window occlusion must complete
the serial with typed retryable/terminal failure, not a successful guessed time. A macOS Rust integration test should
submit identifiable colors across several drawable depths and assert monotonic callback serial/time plus the absence
of feedback before completion. `cocoa_presenter_reports_actual_drawable_presentation_time_and_refresh` pins the
source-level plumbing until that live gate is available.

dd's internal cross-queue Metal fence is not guest-visible explicit synchronization. `fence_begin_render` and
`fence_begin_present` correctly prevent dd's own executor from overwriting an IOSurface while the presenter samples
it, but a standard dmabuf client cannot attach an acquire fence and never receives a release fence. Compose either
`zwp_linux_explicit_synchronization_v1` or the newer DRM syncobj protocol according to the pinned client ecosystem;
do not advertise both without full semantics. Store a typed, single-use acquire point in the surface's pending state,
move it atomically with the buffer on commit, wait/import it before the first GPU read, and reject missing/invalid/
already-consumed fences as the protocol requires. After the last composition/read use completes, export a release
sync-file or timeline point and deliver it before allowing reuse; failure, detach and surface destruction need exact
terminal ownership rules. On macOS this needs an explicit bridge between guest Linux sync-file/syncobj semantics and
host `MTLSharedEvent` generations—never treat fd receipt as signaled. Keep implicit synchronization as a truthful
fallback only for formats/importers that actually provide it. Rust state-machine tests should cover fence-before/
after-buffer ordering, multiple commits, rejected fences and disconnect; a C guest should alternate two dmabufs with
delayed acquire signals and verify no tearing/deadlock plus exact release ordering.
`compositor_explicit_sync_waits_acquire_before_sampling_and_releases_after_gpu_completion` pins the full boundary.

Dmabuf feedback and import now share the narrow ARGB/XRGB dd-tagged pair list; rejected `LINEAR` pairs are no longer
advertised and zero IOSurface ids fail import validation. The advertised magic modifier has low id bits zero,
while validation rejects IOSurface id zero and real buffers encode a dynamic IOSurface id into those modifier bits.
A DRM modifier is a stable layout identifier selected from advertised pairs, not a per-allocation object handle;
overloading it this way prevents a standards client from choosing exactly the advertised value and makes feedback
untruthful. Build feedback and import from one `DmabufImportCaps` table and mechanically test the full pair matrix.
Keep `LINEAR` absent until actual Linux dma-buf fd import exists. Move dynamic host allocation identity into a validated
fd-backed bridge object or an explicitly versioned private protocol/metadata channel, while keeping the modifier a
stable advertised layout. Never advertise the private pair to clients that cannot create that bridge.
`dmabuf_feedback_advertises_only_pairs_that_the_importer_can_accept` pins negotiation closure.

Accepted buffers now pass a shared-capability validator before `notifier.successful`: ARGB/XRGB require exactly one
plane at index zero, empty flags, zero offset, a checked minimum stride and checked u64 extent within the real fd. The
Metal presenter resolves the IOSurface id and reports its live width, height, bytes-per-row and BGRA pixel format; all must exactly
match the guest declaration, so zero, stale and mismatched ids fail. A Rust state matrix covers format, flags, plane
count/index, offset, stride, truncated backing, missing metadata and mismatch. Remaining authenticity is allocation
generation: a newly allocated IOSurface reusing the same numeric id could satisfy old dimensions, because the private
modifier carries no generation/token. Move identity into authenticated fd metadata or a versioned channel, then add C
params/create_immed cases for duplicate/reordered planes and stale-generation reuse.
`compositor_validates_dmabuf_planes_flags_and_backing_metadata_before_success` remains partial on that boundary.

The Smithay shm path has meaningful SIGBUS protection already: mapped accesses install a scoped handler and convert
a fault in the active pool into `InvalidFd`, so this is not a wholly missing feature. The surrounding validation still
needs hardening. `wl_shm_pool.resize(size <= 0)` posts an error but does not return before converting/remapping; pool
creation/remap trusts the declared extent without first comparing `fstat(st_size)`; buffer bounds cast pool `usize`
to `i32` and use hand-ordered signed products. Validate fd type/access mode and real length before mmap, cap size via
the per-client budget, return immediately after every protocol error, and express offset + (height−1)×stride + row
bytes solely with checked `usize` conversions/arithmetic. Resize must be grow-only, reserve budget and verify backing
growth before atomically replacing the mapping; failure leaves the old mapping valid. Seals are useful evidence but
must not be required from portable clients, and truncation can still race, so retain the SIGBUS guard. Add ordinary
Rust boundary tests plus a process-isolated stress child that repeatedly truncates/grows an fd during copies and
proves only the offending client dies/errs while another animates. Never run a deliberate SIGBUS race in-process with
the main test harness. `smithay_shm_pool_validation_prevents_oversized_mapping_truncation_and_sigbus_escape` pins the
validation and containment layers without discarding existing vendored protection.

Smithay teardown is incomplete. `CompositorHandler::destroyed` is not implemented, `BufferHandler::buffer_destroyed`
does nothing, and `ClientData::disconnected` is empty. Role-specific popup cleanup removes only one buffer entry;
there is no authoritative path that removes every `buffers`, `repacks`, `dirty`, title/role/input entry, native
window, IOSurface wrapper, or the per-IOSurface `MTLEvent` pair created by `fence_begin_render`. The legacy server
explicitly calls `fence_drop` on disconnect; the Smithay path does not. Introduce one idempotent
`destroy_surface_state(sid)` used by surface-role destruction and `CompositorHandler::destroyed`, plus a client-owned
resource index so disconnect can destroy all remaining surfaces and executor allocations. Fence/cache keys must
include allocation generation or client ownership, not only a reusable numeric IOSurface id. Add churn tests that
create/destroy thousands of clients and surfaces while asserting bounded map/window/fd/Metal-object counts and no
stale-generation deadlock. `compositor_surface_teardown_reclaims_cpu_gpu_window_and_fence_state` pins the required
hooks and cleanup domains.

Multi-client identity is currently unsafe even before teardown: `surface_id()` returns only
`surface.id().protocol_id()`, but Wayland object ids are scoped to a client. All clients commonly create their first
surface with the same numeric id, while `buffers`, `repacks`, `dirty`, `titles`, `content_types`, `idle_inhibitors`
and the presenter are global maps keyed by that bare `u32`. A second client can therefore replace or clear another
client's cached pixels, metadata, dirty state or native window. Replace the alias with a copyable `SurfaceKey`
containing stable client identity plus object identity/generation; use it through every compositor and presenter API,
and keep the wire `protocol_id` only for diagnostics. Cross-client relationships such as xdg-foreign must retain
explicit authorization rather than weakening this ownership boundary. Add a two-client test deliberately using the
same protocol object ids, commit different colored buffers, destroy/recreate one surface, and prove both windows,
callbacks and caches remain independent. `compositor_surface_keys_include_client_identity_and_generation` prevents
the bare-id representation from returning.

The compositor now has checked per-client and process-wide accounting for live surfaces, retained frame callbacks and
persistent `RepackCache::bgra` bytes. Surface creation reserves before registering and posts a no-memory protocol
failure to only the abusive client; cache replacement computes the complete post-replacement charge before allocating;
callback delivery, cache removal, surface destruction and disconnect refund the exact owner account idempotently. The
two-client protocol journey uses a one-surface limit and proves quota teardown does not reclaim the neighbour. This is
not the whole resource boundary yet: mapped Smithay shm-pool bytes/fds, dmabuf/IOSurface imports, native presenter
objects and executor GPU allocations still need charge tokens, allocation-failure rollback and isolation stress.
`compositor_enforces_per_client_render_resource_budgets` remains partial until those domains are owned.

Commit `06e002e7` also lands the shared eight-value buffer-transform mapping through geometry/UV/damage and fixes
viewport destination sampling plus premultiplied ARGB source-over (XRGB remains opaque). Both former red gates are
normal regressions with asymmetric fixtures. Residual fidelity work is linear-light/color-managed blending, filter
selection, transformed input/opaque regions, complex damage clips, fractional-scale combinations and live GPU parity.

The acceptance matrix must cover at least GTK3/GTK4, Qt5/Qt6, SDL2/SDL3, GLFW, Chromium/Electron, Firefox,
Java/AWT, and representative Rust toolkits on software and accelerated paths where they support them. If X11 is
in scope, add Xlib/XCB, GLX, Wine and XWayland applications. Each cell must test launch, first frame, input, text,
clipboard, menus/dialogs, resize/scale, multiple windows, animation/video, suspend/idle, and clean shutdown—not
merely “a window appeared”.

### 5.2 Additional protocol/API surfaces missing from a literal universal target

The current Smithay global set is a useful modern baseline, not the whole Linux desktop ecosystem. Before claiming
universal compatibility, inventory and either implement or explicitly exclude: `xdg-foreign`/foreign-toplevel
relationships, idle-inhibit, pointer gestures, tablet, keyboard-shortcuts-inhibit, content-type, tearing control,
explicit synchronization, color management, DRM lease (if relevant), session lock, and accessibility/portal
integration. Protocols must be selected from observed toolkit needs and stable Wayland specifications; advertising
unused protocols speculatively creates more conformance liability.

The former `rendering_wayland.rs` file attempted to turn this inventory into gates by searching implementation text;
it has been removed. Each item below needs a behavioral Wayland client/server harness before it is called a test:

- **Linux device-id fidelity:** feedback now uses an explicit Linux `u64` newtype and little-endian eight-byte
  serialization independent of macOS `libc::dev_t`; the Rust wire regression and C guest parser both assert synthetic
  render node 226:128. The remaining evidence gap is running both across the macOS host/guest fd bridge.
- **Modern native-Wayland breadth (completed slice):** commit `47d5dab8` composes pointer gestures, tablet, idle
  inhibit, content type, keyboard-shortcuts inhibit and xdg-foreign as state/delegate/policy slices with a 307-line
  protocol test. `modern_gui_protocol_groups_are_composed_from_vendored_smithay` is now a normal green gate. Other
  protocols named above remain separate scope and must be added only from observed toolkit requirements.
- **X11 compatibility:** there is no XWayland/XWM integration in `dd-compositor`. For a literal arbitrary-GUI
  promise, enable Smithay's XWayland support, supervise the X server, bridge XWM surfaces into the same window/
  focus/clipboard/presentation model, and provide GLX through a supported Mesa/translation path. Otherwise state
  clearly that only Wayland-native applications are in scope.

Likewise, EGL/GLES alone does not satisfy applications linked against desktop `libGL.so`, GLX, OpenCL, VA-API,
VDPAU, or Vulkan extensions outside the minimal profile. The packaging/loader layer needs an executable decision
for each common soname: supported shim, safe software fallback (for example a guest Mesa software renderer into
`wl_shm`), translation layer, or clear unsupported diagnosis. Silent library substitution is not acceptable.

## 6. Detailed remediation plan

### Phase 0 — make completeness measurable

1. Replace each bare `IMPLEMENTED` name list with a generated inventory containing `stub`, `partial`, or `full`,
   supported core version/extensions, parameter restrictions, IR requirements, backend requirements, and tests.
2. Generate the public census documentation and compile-time checks from that inventory. Fail CI if a command is
   advertised without a full/explicitly-partial capability record.
3. Change generated fallbacks to deterministic API-correct failures. Initialize outputs, set GL/EGL error state,
   return `VK_ERROR_FEATURE_NOT_PRESENT`/`VK_ERROR_EXTENSION_NOT_PRESENT` as appropriate, and CUDA's closest
   defined error. Never return success without performing the operation.
4. Add `DD_SHIM_STRICT=1` to abort at the first unsupported call with command, object, thread/context, and recent
   call history. Keep once-per-name logging for exploratory application runs.
5. Capture actual call traces from Chrome, GTK4, glmark2, vkcube, representative Vulkan compute, and CUDA samples;
   use trace frequency plus dependency order to prioritize implementation, not manifest order.

Exit gate: a generated report names every exported call and no unsupported call can silently report success.

### Phase 1 — capability negotiation before more commands

1. Build GL version/extension strings from the full-call inventory. Initially advertise the lowest coherent GLES
   profile; raise it only when its mandatory behavior passes.
2. Build Vulkan instance/device extension, feature, property, format, and limit responses from one device profile.
   Do not expose commands or extensions merely because `vk.xml` contains them.
3. Add a backend capability handshake to the executor transport before EGL/Vulkan device creation. It must cover
   IR commands, formats/usages, shader input language, limits, compute, external surfaces, and synchronization.
4. Make backend selection occur before guest API advertisement and reject incompatible combinations early.

Exit gate: an application cannot select a feature that the chosen executor cannot execute.

### Phase 2 — repair the common shader and coordinate contract

1. Choose one portable shader payload for new IR—SPIR-V is already the Vulkan contract and is accepted by naga.
   Move GL source translation to a deterministic GLSL-ES-to-SPIR-V path with reflection metadata. Remove the
   MSL-bytes-in-`spirv` ambiguity.
2. Represent bind layouts, uniform layout, texture/sampler types, vertex inputs, fragment outputs, specialization,
   and push constants explicitly. Validate reflected requirements against pipeline and bind-group descriptors.
3. Define coordinate conventions in the IR: framebuffer origin, viewport direction, clip-space Y and Z, texture
   upload row origin, front-face winding, and presentation transform. Emit explicit transforms; delete target-id
   and offscreen-history flip heuristics after parity tests pass.
4. Build asymmetric orientation, scissor, winding, depth, texture sampling, mip, sRGB, blending, and matrix-layout
   goldens. Run each identical IR stream through software where possible, bespoke Metal, and wgpu and compare with
   per-format tolerances.

Exit gate: arbitrary supported shaders compile without built-in substitution, and orientation is data-driven.

### Phase 3 — extend IR and executors together

Implement vertical slices, each landing IR encoding, validation, both intended executors, shim lowering, and tests
in one change. Recommended order:

1. texture views/subresources, texture-to-texture copy, blit and multisample resolve;
2. explicit barriers/resource states and host visibility;
3. real compute on the bespoke Metal backend or an explicit decision to retire it for shared-IR compute;
4. indirect draw/dispatch, push constants and dynamic offsets;
5. queries/timestamps and synchronization primitives;
6. depth/stencil completeness, 3D/array/cube textures, mip generation and compressed formats;
7. external IOSurface memory plus a cross-process timeline/event contract.

For each new IR tag, require malformed-stream tests, lifecycle validation, software/mock behavioral tests where
possible, Metal-device tests, wgpu-device tests, and one front-end integration test. Version the wire protocol and
negotiate it; never let a stale guest/backend pair interpret a new tag accidentally.

Exit gate: the selected support profile has no IR operation implemented by only one required executor.

### Phase 4 — finish GL/EGL by coherent profiles

1. Complete GLES2 semantics first: errors, object deletion/reference rules, shared contexts, FBO completeness,
   pixel-store/readback, shader/link failures, sync at swap, resize and surface loss.
2. Add GLES3 in dependency groups: VAOs/instancing/MRT; 3D/array textures and immutable storage; queries/sync;
   UBOs; transform feedback; multisample/blit; then compute/images/SSBOs/barriers.
3. Implement EGL config matching, context attributes/profile negotiation, pbuffer semantics, swap damage/age where
   advertised, and thread/current-context correctness.
4. Run relevant Khronos CTS subsets for the exact advertised profile. Store expected failures only with issue
   links and expiration criteria.

Exit gate: zero generated stubs in the advertised profile and its CTS subset passes at the agreed threshold.

### Phase 5 — make Vulkan a coherent minimal driver, then expand

1. Define a minimal target profile (suggested: Vulkan 1.0 plus only required Wayland/swapchain extensions) and
   stop advertising everything else.
2. Correct memory requirements/types, mapping/coherence, format properties, image layouts and barriers.
3. Implement command-buffer lifecycle and validation, render-pass/subpass dependencies, copies, clears and
   dynamic state required by the profile.
4. Implement real queue submission, binary semaphore and fence state machines, timeouts, acquire/present ordering,
   multiple frames in flight, resize/out-of-date/suboptimal, and swapchain replacement.
5. Add loader-driven tests using the built ICD JSON, then Vulkan CTS subsets and live `vkcube`/sample suites on
   both executors. Expand core versions/extensions only as complete vertical slices.

Exit gate: no advertised Vulkan command is a default stub; loader, CTS subset, resize, and multi-frame WSI gates pass.

### Phase 6 — unify executor and compositor lifecycle

1. ~~Ensure Smithay starts the GPU executor after the exec boundary~~ — **implemented** in `dd-compositor/src/gpu.rs`.
   Next move readiness into a supervisor result/handshake (or separate process) shared by both compositor choices;
   do not treat an atomic “thread spawned” flag as proof the socket bound successfully.
2. Add startup health/capability negotiation and fail visibly if accelerated clients connect without an executor.
3. Replace the dmabuf feedback format-table handoff with a guest-mappable memfd/transport abstraction; encode the
   Linux protocol's 64-bit device value explicitly. Test bind, mmap, parse, buffer creation and destruction inside
   a real guest.
4. Validate Smithay's native Cocoa path: accelerated Chrome and Vulkan, resize/scale, input, cursor, clipboard/IME,
   popup windows, multiple windows, disconnect/reconnect, and fallback behavior.
5. Make Smithay/default-backend flips only after a combination matrix is green; retain explicit legacy escape
   hatches for one release cycle.

Exit gate: all supported compositor × executor combinations either pass the matrix or are rejected at startup.

### Phase 7 — close application-level blockers and harden CI

1. Diagnose multi-process Chrome at the Mojo invitation/channel state level with targeted Chromium VLOG and syscall
   correlation; retain the already-passing cross-process primitive gates as regression coverage.
2. Establish serialized macOS hardware runners for Metal/IOSurface tests. Keep headless IR tests on every platform,
   but do not use them as evidence of device-level presentation.
3. Add golden images for Chrome UI/content, GTK4, GLES samples, and Vulkan; add compute numerical comparisons for
   Vulkan and CUDA. Include resize, HiDPI, orientation, alpha, clipping, and 500+ frame stability.
4. Run `make mac-crates` on shared IR/presentation changes and make the shim census/strict trace/loader tests
   ordinary CI gates. Archive capability report, trace, image, and backend/device identity with failures.

Exit gate: named unmodified workloads are reproducible from clean workspaces and failures identify the layer.

## 7. Recommended priority and non-goals

The shortest path to trustworthy rendering is: truthful advertisement and strict failures; one shader/coordinate
contract; executor wiring for Smithay; coherent GLES2 and Vulkan-minimal profiles; then breadth. Porting hundreds
of registry commands before these foundations would increase the apparent surface without making it reliable.

Do not pursue PRIME/IOSurface mechanisms for the historical Chrome multi-process blank-content issue without a
trace showing that Chrome uses that path; existing investigations observed no PRIME calls. Do not flip Smithay or
wgpu defaults based on software `PngPresenter` evidence alone. Do not call ABI census tests conformance tests.

## 8. Verification performed for this audit

- Read the current manifests, generators, shim implementations, shared IR, both executors, compositor startup,
  and existing rendering documents.
- Recounted GL/Vulkan manifest commands and `IMPLEMENTED` entries directly from the current tree.
- Ran `cargo test -p dd-shim-gl -p dd-shim-vk --lib -- --nocapture`: GL 16/16 and Vulkan 3/3 passed.
- Did not run a live guest, Chrome, GTK, Vulkan loader/ICD, CUDA workload, Metal device test, or image comparison.
  Those are explicitly left as validation gates above; source and unit-test evidence cannot prove them.

## 9. Rust implementation map and upstream references

The project should reuse mature state machines and specifications without copying their C++ architecture into
Rust. References answer “what semantics are required”; dd's Rust types should enforce ownership, state and wire
validation locally.

### 9.1 Reference provenance must be pinned

`third_party/smithay-0.7.0` is present and patched locally. MoltenVK is **not** currently vendored in `reference/`
or `third_party/`, although `dd-shim-vk` comments cite MoltenVK classes and source files. That makes line-level
citations impossible to reproduce and makes future upstream changes invisible.

Add `reference/moltenvk` as a pinned read-only source checkout or submodule, recording commit, license and origin in
`reference/LOCK.md`. Pin the matching Vulkan-Headers and Vulkan-Loader revisions as well. Reference updates should
be their own reviewable commits: update the pin, regenerate manifests, run a semantic-diff checklist, and only then
port selected behavior. Never modify a reference tree to make dd tests pass; dd adaptations belong in dd crates or
an explicitly documented `third_party` patch series.

Suggested reference ownership:

| Problem | Primary authority | Rust implementation location |
|---|---|---|
| Vulkan API validity and synchronization | Vulkan spec + Vulkan-Headers `vk.xml` | generated inventory/types plus `dd-shim-vk` state machines |
| Loader/ICD ABI and proc-address rules | Vulkan-Loader `LoaderDriverInterface.md`, loader tests | `dd-shim-vk/src/icd.rs`, dispatchable handle wrapper |
| Vulkan-on-Metal lowering decisions | pinned MoltenVK, especially MVK memory/image/queue/command/WSI objects | `dd-shim-vk` lowering into explicit `dd-gpu` IR; host backends stay API-neutral |
| SPIR-V validation and translation | SPIR-V spec/tools + naga | shared shader module/reflection crate used by both front ends and executors |
| Wayland protocol state | protocol XML + vendored Smithay | `dd-compositor` handler composition, avoiding duplicate hand-written state machines |
| CUDA/PTX behavior | CUDA docs plus pinned ZLUDA reference | typed CUDA state and PTX lowering in `dd-shim-cuda`, `dd-gpu::ptx` |

### 9.2 Rust shape for Vulkan completeness

Avoid one global registry guarded by one mutex as the permanent object model. Introduce typed generational handles
and parent-owned arenas:

```rust
struct Handle<T> { index: u32, generation: u32, _kind: PhantomData<T> }
struct DeviceState {
    buffers: Arena<Buffer>, images: Arena<Image>, memories: Arena<DeviceMemory>,
    descriptors: Arena<DescriptorSet>, command_buffers: Arena<CommandBuffer>,
    sync: SyncState,
}
enum CommandBufferState { Initial, Recording(Recorder), Executable(Stream), Pending(SubmissionId), Invalid }
enum ImageState { Unbound, Bound { memory: MemoryRange }, External { surface: SurfaceId } }
```

Every Vulkan entry point should follow one path: ABI-safe pointer/count validation; typed handle lookup and parent
validation; Vulkan state transition; lowering to backend-neutral IR; precise `VkResult`. Keep unsafe code in a
small ABI module and make state/lowering safe Rust. Use `ash::vk` only for ABI types/constants; do not let raw
handles or pointers leak into executor code.

Port semantic verticals from MoltenVK in this order:

1. `MVKDeviceMemory`/`MVKBuffer`/`MVKImage`: requirements, bindings, coherent/non-coherent ranges, subresources.
2. `MVKCommandBuffer` and command objects: recording lifecycle and immutable executable command streams.
3. `MVKQueue` submission: dependency graph, fence/semaphore transitions and completion callbacks.
4. `MVKRenderPass`/pipeline descriptors: attachment layouts, load/store/resolve and compatibility.
5. `MVKSwapchain`: per-image availability, acquire/present state, resize/surface loss and retirement.

Do not port MoltenVK's Objective-C lifetime mechanisms or Metal object caching literally. Model them with owned
Rust records, `Arc` only for genuinely shared immutable resources, and explicit submission retirement. Compare
observable Vulkan behavior and Metal encoding, not class names.

### 9.3 Rust shape for GL/EGL completeness

Split the current broad state module into typed domains: `EglDisplay`, `EglContext`, `EglSurface`, `ShareGroup`,
`GlObjects`, `GlBindings`, and `GlError`. Store current context per thread and shared object namespaces in a
`ShareGroup`. Each public C ABI function should be a thin validator calling a safe method that either mutates state
and emits IR or records the first GL/EGL error.

Generate function metadata from Khronos XML, but hand-implement semantics by coherent version profile. A capability
builder must derive `GL_VERSION`, GLSL version, extensions, formats and limits from completed method groups and the
negotiated backend. Shader compilation should produce a typed artifact containing SPIR-V, reflection, translated
diagnostics and source identity; never overload a `Vec<u32>` field with MSL bytes.

### 9.4 Rust shape for compositor/executor integration

Create one supervisor-owned service graph instead of starting the executor inside the legacy compositor branch:

```text
DisplaySupervisor
  ├─ GpuExecutorService { capabilities, socket, health }
  ├─ CompositorService::Legacy | ::Smithay
  └─ CocoaPresenter (main thread)
```

Prefer a separate executor process if crash isolation and backend restart are product requirements; otherwise a
thread owned before compositor selection is sufficient. Pass a typed startup descriptor rather than relying on
independent environment-variable reads. Smithay handlers should send `PresentRequest` values over a bounded channel
and receive completion/release events; no handler should call Metal or wgpu directly.

For additional Wayland protocols, first check whether the pinned Smithay already implements the protocol module
(the current vendored tree includes many currently unused modules such as pointer gestures, tablet, idle inhibit,
content type, explicit DRM syncobj, xdg foreign and XWayland support). Compose and test those handlers rather than
writing wire dispatch manually. A Smithay module existing is not completion: dd must still supply host policy,
AppKit integration, focus/security decisions and live toolkit tests.

### 9.5 Gap lifecycle used by future agents

Every gap should carry one of these states:

- `missing`: no implementation exists;
- `partial`: code exists but a required semantic domain is absent;
- `implemented/unverified`: intended code exists but its named live/conformance gate has not run;
- `closed`: current-tree implementation and the full named evidence gate pass;
- `regressed`: previously closed evidence fails on the current tree.

To close or remove a row, record the implementation commit/path, exact command or live reproduction, result,
backend/compositor/device identity, and date. A unit test may close a unit-level gap but cannot close a live Metal,
cross-process, toolkit or conformance gap. When code lands without the broad gate, update the suggested solution to
describe only the remaining validation/fidelity work instead of deleting the issue.

### 9.6 Periodic refresh procedure

1. Inspect rendering changes since the last reviewed revision with `git log`/`git diff`; keep transient output in
   review notes, not this file.
2. Inspect every changed shim manifest, capability inventory, advertised feature/extension, IR tag, executor match,
   compositor global and test. Search for generated stubs and success fallbacks.
3. Re-run the narrow crate tests, then `make mac-crates` for shared types/executors/compositor changes.
4. Re-run only the live gates affected by the diff, but do not infer unrun matrix cells from neighboring cells.
5. Update counts, states and solutions here. Delete claims contradicted by the current tree and preserve useful
   historical evidence in dated rendering notes.
6. Review reference pins for upstream movement. Update references separately; never mix a large upstream refresh
   with behavioral dd changes.

## 10. Rendering test backlog in `dd-tests`

`dd-tests/guests/gui_matrix` is the application-facing regression layer. Its probes must validate observable
results (pixels, protocol events, errors and lifecycle), not merely call functions. The matrix already covers raw
xdg/shm/dmabuf frames and many EGL draw, copy, blend, texture, FBO, state-churn, resize and swap cases. Keep the
orphan-source guard in `dd-tests/tests/gate_invariants.rs` green so every new probe is built or explicitly excluded.

New in this audit:

- `gui_egl_capability_truth.c` checks that EGL version/API/extensions, config renderable and conformant bits,
  context creation and error clearing agree. Default mode must reject ES3 with `EGL_BAD_MATCH`; `DD_SHIM_ES3=1`
  must both advertise and create ES3. This closes a test-coverage omission, **not** GLES3 completeness.
- `dd-tests/tests/rendering_ir.rs` provides the exhaustive shared-IR corpus and malformed/lifecycle checks described
  in §3. It should grow in the same change as any `Cmd`, `Enc`, descriptor enum or wire encoding.
- `dd-tests/tests/rendering_backends.rs` contains only executed behavior: a software clear/readback/present pixel
  journey and a real Unix-socket executor failure-acknowledgement test.
- The former `rendering_wayland.rs`, `rendering_surface.rs`, and `rendering_ledger.rs` source-search/meta-tests were
  removed. Their findings remain review backlog only until replaced by Rust/C clients that drive observable behavior.
- `gui_egl_error_lifecycle.c` is an explicit red conformance probe for invalid enum/value/operation errors,
  first-error retention, clear-on-read and non-mutation. Source inspection confirms the current Rust shim's
  `glGetError` returns `GL_NO_ERROR` unconditionally and many invalid calls silently return or mutate state, so
  this probe is built but not part of the default green run until that semantic gap is implemented.
- `gui_egl_sharegroup_threads.c` and the normal Rust context gate now cover the model landed in `bf5a1a7f`: unique
  contexts, share-group visibility/isolation, cross-context deletion, per-thread currentness and error lifecycle.
  Surface draw/read identity stays red under the separate surface-object gate.
- `gui_vk_capability_truth.c` drives the ICD negotiation/proc-address ABI without Vulkan headers or `libvulkan`.
  Commit `d48c7f44` now caps advertisement at Vulkan 1.0, rejects newer application requests and gives generated
  unsupported commands deterministic failures. That closes negotiation falsehood, not mandatory Vulkan 1.0 bodies.
- Khronos-derived Vulkan/GL inventory manifests remain useful generated audit inputs, but manifest/source membership
  is not runtime correctness evidence. Profile claims must be tested through loader/API calls and CTS-style behavior.
- CUDA unsupported-PTX behavior should remain covered by the driver crate's executed ABI tests; no source-string
  assertion is accepted as evidence.

Next probes to add, ordered by the gaps they can turn into evidence:

1. Extend `gui_egl_error_lifecycle` across textures, FBOs and draw validation after the foundational error state
   lands; keep asserting first-error retention and rejection without state mutation.
2. Extend `gui_egl_sharegroup_threads` with deletion-while-referenced, shared shaders/programs/textures and
   thread-local GL error state after typed context/share-group storage replaces the global state.
3. `gui_egl_context_surface_matrix`: draw/read surface combinations, surfaceless context, pbuffer, unbind/rebind,
   destroy-current behavior and termination/reinitialization.
4. `gui_gl_object_lifetimes`: delete bound buffers/textures/programs/FBO attachments, name reuse and queued-draw
   snapshots; verify pixels and `glIs*`/query behavior.
5. `gui_gl_sync_visibility`: upload/draw/readback across flush, finish and fence waits; repeat across contexts and
   frames to expose false-already-signaled synchronization.
6. `gui_wayland_surface_tree`: nested sync/desync subsurfaces, viewport, scale and all buffer transforms with
   asymmetric pixels; include destroy/reparent and parent-commit ordering.
7. `gui_wayland_popup_input`: popup constraint adjustment outside parent bounds, grabs, dismissal, focus and native
   child-window placement through the Cocoa presenter.
8. `gui_dmabuf_feedback_guest`: bind v4 feedback, mmap/parse the format table in the real guest, validate 64-bit
   `main_device`, create a buffer and observe release. This is the gate required to remove the dmabuf warning.
9. Extend `gui_vk_capability_truth` through a real Vulkan loader with extension/feature/format enumeration and a
   generated mandatory-core census for the advertised version; no unsupported operation may return `VK_SUCCESS`
   or leave required outputs untouched. Passing the current version check alone does not certify Vulkan 1.0.
10. `gui_vk_swapchain_lifecycle`: two/three frames in flight, finite acquire timeout, semaphores/fences, resize,
    `OUT_OF_DATE`/`SUBOPTIMAL`, `oldSwapchain`, surface loss and result arrays.
11. `gui_vk_resource_lifetimes`: invalid parents, double destroy, memory alignment/bounds, non-coherent ranges,
    image subresources/layout transitions and command-buffer state transitions.
12. `gui_backend_parity`: serialize identical IR fixtures and compare bespoke Metal with wgpu for asymmetric
    orientation, blend, depth/stencil, sRGB, formats, copies, render-to-texture and compute.
13. `gui_multiclient_isolation`: concurrent shm, EGL and Vulkan clients; crash one during commit/submission and
    prove the others continue, with bounded fd/memory growth.
14. Toolkit journeys for GTK3/4, Qt5/6, SDL2/3, GLFW, Chromium/Electron and Firefox, each asserting first frame,
    input/text, clipboard, menus, resize/scale, child windows and clean shutdown with archived screenshots/traces.

Every hardware/live probe needs an explicit skip result when prerequisites are absent; the runner must treat an
all-skipped requested gate as failure. Record which compositor and executor each probe covers so software Smithay
success cannot accidentally greenlight accelerated Smithay.

## 11. Rendering behavior backlog

This is an engineering backlog, not executable evidence. The former source-inspection sentinels were removed because
searching Rust files for names or snippets does not prove rendering behavior. A row becomes an executable gate only
after it has a Rust or C harness that drives the public ABI/protocol/socket/backend, observes state, errors, timing or
pixels, and fails against the broken behavior. Do not add tests that read implementation source text.

<!-- rendering-gap-ledger:start -->
| Required behavioral regression | State | Finding | Evidence required to close |
|---|---|---|---|
| `vk_abi_manifest_contains_every_core_command_in_the_pinned_registry` | missing | ABI registry is 19 Vulkan 1.4 core commands behind pinned Khronos XML | Regenerate ABI/types, review signatures, normal + loader census green |
| `vk_advertised_core_has_real_implementations_for_every_mandatory_command` | partial | truthful Vulkan 1.0 still has 55/137 mandatory generated failures/stubs | Implement all advertised core semantics and pass CTS subset |
| `vk_wsi_validates_surface_handles_and_swapchain_create_info` | implemented | Shared capability validation rejects stale surfaces and incompatible swapchain requests atomically | Rust ABI negative create/query matrix green; retain as regression |
| `vk_swapchain_tracks_image_ownership_timeouts_and_retirement` | implemented | Explicit image/swapchain states enforce ownership, timeout results, acquire sync, retirement and loss | Rust ABI multi-frame lifecycle regression green; retain as regression |
| `vk_present_reports_failures_without_consuming_unsent_frame_state` | implemented | Present validates atomically, maps delivery errors, fills `pResults`, and commits waits/IR/ownership only on success | Rust ABI delivery-fault/retry regression green; retain as regression |
| `vk_image_layout_barriers_track_subresources_and_queue_ownership` | missing | images and barrier APIs have no layout/access/ownership state | Per-subresource transition model plus hazard/ownership tests |
| `vk_transfer_commands_preserve_every_region_subresource_and_layout` | partial | shared IR/backends have subresource copy+blit, but Vulkan lowering and remaining region/resolve fields are absent | Shim lowering plus buffer/image/resolve/clear validation matrix |
| `vk_shader_modules_validate_spirv_entries_specialization_and_interfaces` | missing | malformed SPIR-V and incompatible interfaces are forwarded unchecked | Validated/reflected module records and negative pipeline corpus |
| `vk_robust_buffer_access_is_advertised_only_with_zeroing_and_bounds_guarantees` | contradictory | robustBufferAccess is true without complete zero/discard bounds semantics | Disable now or prove every access path across all executors |
| `opt_in_gles3_has_real_implementations_for_every_mandatory_command` | partial | opt-in GLES3 has 112/246 mandatory stubs | Complete mandatory ES3 groups and relevant CTS |
| `dmabuf_feedback_serializes_an_explicit_linux_u64_device_id` | partial | explicit Linux-u64 LE serialization and real-wire Rust/C mmap parsers are implemented; the macOS/guest runtime gates have not yet run on this host | Run the Rust recvmsg+mmap regression and C guest probe through the macOS compositor/engine, then retain both green |
| `x11_only_gui_apps_have_an_xwayland_bridge` | missing | no XWayland/XWM/GLX compatibility path | Supervised XWayland journey with input/clipboard/rendering |
| `gles_generated_names_binding_and_deletion_follow_object_lifetimes` | contradictory | generated names are prematurely live and deletion leaves stale bindings | Reserved/live generations with lazy bind and complete detachment tests |
| `gles_shader_program_attachment_detach_and_delete_pending_are_consistent` | missing | shader attachments and deferred deletion are not modeled | Strong attachment sets, delete-pending ownership and query tests |
| `gles_pixel_store_and_texture_upload_validation_is_atomic_and_checked` | contradictory | invalid pixel-store/upload state uses unchecked arithmetic and mutates textures | Checked format/layout table, immutable mip storage and negative corpus |
| `gles_framebuffer_completeness_reflects_attachment_state_and_blocks_draws` | partial | color-only FBOs compute missing/undefined attachment status and block clear/draw; broader attachment vocabulary and read/blit guards remain | Depth/stencil/layer/sample compatibility plus read/blit negative matrix |
| `gles_draw_calls_validate_all_inputs_before_snapshot_or_recording` | partial | core array/index draws validate program, FBO, enabled vertex ranges and index source atomically | Mapped state, negotiated limits and faithful instanced/base-vertex IR fields |
| `gles_readpixels_validates_pack_layout_and_preserves_output_on_error` | contradictory | invalid readback zero-fills output and ignores pack layout | Checked pack-state readback after completion synchronization |
| `gles_sync_objects_track_real_submission_completion_and_wait_results` | missing | sync APIs have no objects or cross-process completion serial | Context serials, typed acknowledgements and timeout/lifetime parity tests |
| `gles_query_objects_track_targets_availability_and_asynchronous_results` | missing | query names have no target lifecycle, readiness or backend result | Typed queries, resolve serials and negotiated backend query types |
| `egl_query_context_rejects_destroyed_handles_without_mutating_output` | contradictory | context query accepts destroyed handles and fabricates values | Live-handle validation, preserved outputs and `EGL_BAD_CONTEXT` |
| `vulkan_shader_translation_failure_never_falls_back_to_builtin_rendering` | contradictory | Metal/wgpu silently replace failed Vulkan shaders with builtins | Tagged shader payloads and propagated compile/link failure |
| `executor_reconnect_replays_complete_residency_or_reports_api_loss` | contradictory | reconnect reset is never consumed, so new executor lacks resident resources | Generation replay or explicit GL/Vulkan loss with restart fault tests |
| `executor_enforces_every_negotiated_limit_before_decoding_or_allocating` | contradictory | handshake limits other than frame bytes do not constrain replay/backend allocation | Shared ReplayLimits validation and fallible exact-boundary tests |
| `executor_accounts_cumulative_residency_and_object_counts_per_connection` | missing | unlimited individually legal GPU resources can exhaust a connection/process | Per-connection and global charge/refund budgets with disconnect stress |
| `compositor_surface_teardown_reclaims_cpu_gpu_window_and_fence_state` | partial | Smithay surface and disconnect destruction now share idempotent CPU/cache/callback/window cleanup; client-owned executor resources and in-flight GPU fences still need ownership teardown | Add executor-owner reclamation and completion-fence retirement to the existing disconnect journey |
| `compositor_surface_keys_include_client_identity_and_generation` | implemented | live surfaces use Wayland `ObjectId` (client + generation) to allocate monotonic presenter/cache ids; an ordinary two-client protocol test deliberately collides local ids and proves isolation | Keep `compositor_surface_identity_is_client_owned_generational_and_teardown_is_exact_once` green |
| `compositor_enforces_per_client_render_resource_budgets` | partial | checked per-client/global surface, retained-callback and CPU repack-cache accounting now reserves and refunds by owner; shm mappings/fds, imports, presenter objects and executor allocations remain uncharged | Extend owner tokens to remaining domains and add multi-domain isolation/rollback stress |
| `compositor_releases_buffers_only_after_the_last_cpu_or_gpu_use` | partial | generation-tagged uses give shm copy-complete exact-once release and retain zero-copy across failure; Metal has no actual GPU-completion token | Add presenter completion tokens and out-of-order zero-copy retirement tests |
| `compositor_retains_presentation_feedback_across_retryable_failure_only` | contradictory | failed present retains callbacks for retry but immediately discards its feedback | Typed retry class with coupled callback/feedback/resource terminal policy |
| `compositor_routes_each_surface_through_its_actual_output_membership` | contradictory | outputs are advertised but surfaces have no enter/leave, selected scale, fullscreen target, or present route | Per-surface membership state and two-output event/render journey |
| `compositor_output_hotplug_migrates_surfaces_and_reconfigures_scale` | missing | extra outputs are append-only globals with no surface migration | Transactional removal, fallback placement, scale/configure and pending-frame tests |
| `software_backend_applies_srgb_transfer_functions_around_filtering_and_blending` | contradictory | sRGB formats are sampled/blended as encoded bytes in software | Linear-light transfer helpers and format-specific golden pixel tests |
| `cocoa_presenter_reports_actual_drawable_presentation_time_and_refresh` | missing | Metal returns Delivered with no drawable completion timestamp or target refresh | Presented-handler serial/timing plumbing plus live macOS ordering test |
| `compositor_negotiates_surface_color_and_converts_to_the_target_output_profile` | missing | surface/presenter path carries no color description, output profile, or HDR policy | Color protocol plus linear composition and ICC/HDR output conversion fixtures |
| `compositor_honors_input_and_opaque_regions_through_surface_transforms` | missing | decoded surface regions do not affect hit-testing, clipping, or occlusion | Shared transformed-region algebra with shaped-surface input/pixel tests |
| `compositor_minimize_and_occlusion_control_native_visibility_and_frame_pacing` | contradictory | set_minimized is an empty handler and hidden/occluded presentation is unmodeled | Visibility state, presenter hide/occlusion hooks, reveal and pacing journey |
| `compositor_presentation_feedback_uses_one_backend_evidence_record_per_output_frame` | contradictory | presenter timing/serial are discarded and MSC/60Hz/vsync are invented per surface | Shared per-output delivered-frame evidence across the paced tree |
| `compositor_explicit_sync_waits_acquire_before_sampling_and_releases_after_gpu_completion` | missing | internal MTLEvent ordering is not a Wayland acquire/release fence contract | Explicit-sync/syncobj state plus Linux-fence↔MTLSharedEvent bridge journey |
| `dmabuf_feedback_advertises_only_pairs_that_the_importer_can_accept` | partial | feedback now exposes only dd-tagged ARGB/XRGB pairs and rejects malformed/zero ids, but real IOSurface allocation generation and backing metadata are not represented | Share allocation-generation metadata with the importer and run positive/negative GPU-backed guest probes |
| `compositor_validates_dmabuf_planes_flags_and_backing_metadata_before_success` | partial | shared caps enforce single-plane layout/flags/checked fd extent and exact live IOSurface width/height/row bytes/BGRA format; allocation generation is unavailable | Authenticate allocation generation and add stale-id C protocol regression |
| `smithay_shm_pool_validation_prevents_oversized_mapping_truncation_and_sigbus_escape` | partial | SIGBUS guard exists, but fd extent, invalid resize and checked bounds remain unsafe | fstat/caps/checked mapping plus isolated truncation-race regression |
<!-- rendering-gap-ledger:end -->
