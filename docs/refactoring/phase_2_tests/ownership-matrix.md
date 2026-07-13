# Phase 2 test ownership matrix

This is the destination map for the current tree. Paths are planning targets, not completed moves.

## Current `dd-tests` surfaces

| Current surface | Destination owner | Planned destination | Reason |
|---|---|---|---|
| `src/harness/**`, `diag.rs`, generic report/evaluation model | `dd-tests` | retain and narrow under `src/harness` | reusable engine/process test infrastructure |
| `src/cases/**` | `dd-jit-darwin` | `dd-jit-darwin/tests/engine_matrix/**` | instruction, syscall, ABI and engine behavior |
| `guests/completeness`, `ext_abi`, `ext_*`, root engine probes | `dd-jit-darwin` | `dd-jit-darwin/testdata/guests/**` | fixtures are coupled to the C JIT/runtime implementation |
| `tests/suite.rs` and `src/main.rs` aggregate matrix | `dd-jit-darwin` | crate integration test plus optional local runner example/bin | the engine owns the matrix; helper crate must not own product pass/fail |
| `tests/forkserver.rs`, `overlay.rs`, `pcache.rs`, `nonpie_dladdr.rs` | `dd-jit-darwin` | `dd-jit-darwin/tests/**` | direct engine/runtime features |
| `guests/forkserver`, `overlay`, `pcachex`, `procexe` | `dd-jit-darwin` | matching `testdata/guests` groups | move with their tests atomically |
| `compliance/ltp/**` | `dd-jit-darwin` | `dd-jit-darwin/compliance/ltp/**` | Linux syscall/ABI compatibility, not generic harness behavior |
| `src/scenario/**`, `src/scenarios/**`, `src/bin/scenarios.rs` | `dd-daemon` | `dd-daemon/tests/scenarios/**` plus a crate-owned runner | boots/drives Docker API daemon and real images |
| `scenarios/docker*.sh`, compose, real-image, networking scripts | `dd-daemon` | `dd-daemon/testdata/scenarios/**` | daemon/container product journeys |
| scenario database/language/web/toolchain image catalog | `dd-daemon` | `dd-daemon/tests/scenarios/catalog/**` | PostgreSQL and other images test daemon image/container behavior |
| `tests/rendering_ir.rs` | `dd-gpu` | `dd-gpu/tests/ir_contract.rs` | exhaustive IR encode/decode/replay contract |
| software-backend half of `tests/rendering_backends.rs` | `dd-gpu` | `dd-gpu/tests/software_present.rs` | software executor capability and pixels |
| transport-ack half of `tests/rendering_backends.rs` | `dd-shim-common` | `dd-shim-common/tests/transport_ack.rs` | `ExecConn` owns acknowledgement semantics |
| `src/cases/ext/gpu_render_ir.rs` | split by boundary | host IR assertions to `dd-gpu`; guest API fixtures to shim crates | avoid a second GPU conformance owner |
| `guests/gui_matrix/gui_egl_*` | `dd-shim-gl` | `dd-shim-gl/testdata/egl-gles/**` | guest EGL/GLES ABI and pixel semantics |
| `guests/gui_matrix/gui_vk_*` | `dd-shim-vk` | `dd-shim-vk/testdata/vulkan/**` | Vulkan loader/ICD capability and WSI semantics |
| `guests/gui_matrix/gui_dmabuf_*`, `gui_shm_frame`, `gui_xdg_ack` | `dd-compositor` | `dd-compositor/testdata/wayland-clients/**` | Wayland protocol/composition behavior |
| `guests/shader_translate/**` | shim owning input language; Metal execution in `dd-gpu-wgpu` | crate-local testdata | GLSL belongs to GL shim, SPIR-V to Vulkan, PTX to CUDA; backend replay belongs to executor |
| `tests/gate_invariants.rs` | split | each invariant beside its owning runner; root CI checks composition | current file mixes engine coverage, GUI registration and removed benchmark policy |
| `src/bench_gates.rs`, `src/bin/bench.rs`, `guests/bench/**` | none | delete under approved benchmark cleanup | benchmark island is not correctness ownership |

## Existing crate-local tests to retain

| Owner | Retained responsibility |
|---|---|
| `dd-cli` | parsing, workspace persistence, daemon-free launch configuration and user command behavior |
| `dd-client` | Docker wire/view-model conversion and Unix-socket client behavior |
| `dd-daemon` | API routing, build/image/container/network/volume lifecycle, state recovery, events, exec/attach |
| `dd-images` | reference/auth/registry, archive round trips, layer safety, build cache and discovery |
| `dd-jit` | host-neutral builder/runtime/container API and pump/handle semantics |
| `dd-jit-darwin` | engine build, launch wire, guest selection, all translated guest behavior |
| `dd-gpu` | IR, wire, limits, software backend, resource lifetime and host-neutral replay |
| `dd-gpu-wgpu` | Metal/wgpu lowering, real shader execution, Vulkan/CUDA host replay, IOSurface seam |
| `dd-shim-common` | transport framing, completion and shared guest memory registration |
| `dd-shim-gl` | EGL/GLES state/error/context/share-group/translation and guest ABI |
| `dd-shim-vk` | loader/ICD, object/command/WSI semantics and guest ABI |
| `dd-shim-cuda`, `dd-shim-cudart` | driver/runtime API, PTX launch, registration, dlopen ABI |
| `dd-display` | legacy compositor and presenter seam while it remains supported |
| `dd-compositor` | Smithay protocols, composition, input, output, dmabuf and presentation lifecycle |
| `dd-term-core` | terminal parsing/grid/layout/input/session/workspace/PNG behavior |
| `dd-gui` | GUI model/controller behavior; native interaction requires the macOS GUI gate |

`dd-tests` should finish with tests only for selection, provisioning, timeout, result normalization,
native-oracle comparison, executed-count enforcement, and helper error reporting.
