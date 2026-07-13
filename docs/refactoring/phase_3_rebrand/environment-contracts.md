# Environment producer/consumer contracts

This is the semantic rename authority for supported environment families. The implementation inventory
must expand each family to exact paths from the current tree. Variables removed in phase 1 never receive a
Husklet replacement.

| Contract/family | Producers | Consumers | Proposed naming/rule |
|---|---|---|---|
| daemon socket | CLI, GUI, tests/tools/operator | daemon and `dd-client` | `DDOCKERD_SOCK` → `HL_DAEMON_SOCK`; update every process together |
| image/state/volume roots | CLI workspace daemon, GUI, scenarios, Make/operator | daemon/images/test provisioning | `HL_IMAGES`, `HL_STATE`, daemon `HL_VOLUMES_DIR` |
| JIT artifact directory | CLI/GUI/package/tests/operator | `dd-jit-darwin::Guest` | `DDJIT_DIR` → `HL_JIT_DIR`; poison old default during gate |
| checkpoint/restore | CLI launcher/tests | Rust launch + C engine | `HL_CHECKPOINT_DIR`, `HL_RESTORE_DIR` lockstep |
| typed guest configuration | Rust builder/configfd | engine C target/container/syscalls | `HL_ROOTFS`, `HL_CWD`, `HL_UID/GID`, `HL_HOSTNAME`, limits/network/publish; prefer typed wire over ambient env where already available |
| guest env payload | Rust spawn config | engine exec/proc environment | `DD_GUEST_ENV` and exact/escape companions → `HL_GUEST_ENV*`; preserve encoding tests |
| display socket | CLI launcher/operator | display/compositor startup | `HL_DISPLAY_SOCK`; distinguish from standard `WAYLAND_DISPLAY` |
| host GPU executor socket | CLI/display/compositor/operator | host executor bind | `DD_GPU_EXEC_SOCK` → `HL_GPU_EXEC_SOCK` |
| guest GPU endpoint | integration provider/tests | common/GL/Vulkan/CUDA/CUDART shims | `DD_GPU_EXEC` → `HL_GPU_EXEC`; mount path changes atomically |
| GPU Mach bridge | test/operator override | display bridge and engine/client sender | `DD_GPU_BRIDGE_NAME` → `HL_GPU_BRIDGE_NAME`; default service changes same wave |
| GPU backend select | package/operator | wgpu/display/compositor | `DD_GPU_BACKEND` → `HL_GPU_BACKEND`; accepted values unchanged unless separately versioned |
| GPU pool/IOSurface | CLI launcher/integration | engine/display/executor | `DD_GPU_POOL`, `DD_GPU_IOSURFACE` → `HL_GPU_POOL`, `HL_GPU_IOSURFACE` |
| CUDA device identity | build scripts/launcher/operator | C and Rust CUDA/NVML shims | `HL_CUDA_NAME/CC/VRAM/VRAM_BYTES/DRIVER/NVML/DRIVER_CUDA` |
| shim strict/debug/capability | tests/operator | all Rust shims and legacy C oracle | `HL_SHIM_*`; remove legacy-oracle-only controls with oracle retirement |
| Vulkan ICD test override | test runner | C capability fixture | `DD_VK_ICD_LIBRARY` → `HL_VK_ICD_LIBRARY`; standard `VK_*` loader variables stay unchanged |
| Vulkan WSI test switch | tests | Vulkan shim | `DD_VK_NO_WL_PRESENT` → `HL_VK_NO_WL_PRESENT` if retained as a supported test hook |
| display mode/capability | launcher/tests/operator | legacy and Smithay paths | `HL_DISPLAY_SMITHAY/DMABUF/HIDPI/FRACTIONAL_SCALE/...`; remove augmenter with feature |
| display input/window policy | tests/operator | Cocoa and Smithay routing | `HL_DISPLAY_INPUT_DEBUG`, mirror geometry, popup windows, decorations; both paths together |
| render/shader/IR diagnostics | tests/operator | GL shim, display Metal, wgpu | `HL_RENDER_*`, `HL_SHADER_*`, `HL_GPU_DUMP_*`, `HL_IR_*`; classify owner/expiry first |
| golden update/tolerance | CI/operator | display/wgpu golden tests | `HL_UPDATE_GOLDENS`, `HL_GOLDEN_*`, `HL_WGPU_*`; test-only public contract |
| app screenshots | `dd-gui/mac/shot.sh` | GUI/terminal | retain supported `HL_SHOT*`; remove producer-less `DD_TERM_*` controls before rebrand |
| package version/sign/notary | Make/release secrets | bundle/notary scripts | `HL_VERSION`, `HL_SIGN_*`, `HL_NOTARY_*`; update CI secrets/docs atomically |
| bundled dependency paths | Nix/Make | package script | `HL_GTK4`, `HL_LIBRSVG`, `HL_GDK_PIXBUF`, schemas/icons/libxkbcommon |
| scenario controls | developer/CI | daemon-owned runner after phase 2 | `HL_SCEN_JOBS`, `HL_SCEN_PROFILE`, `HL_IMAGES`, `HL_DAEMON` |
| JIT supported diagnostics/A-B | developer/test bridge | C engine | prefix retained allowlist with `HL_JIT_*`; never blindly flatten collisions |

## Variables that are constants, not environment

Search results include many `DD_*` C/Rust constants: BPF/seccomp/futex masks, ABI magics, capacities,
format codes, include guards and private table sizes. They are symbol-namespace work, not environment
contracts. Rename them in R2 or leave external-standard constants untouched; do not add them to launch
configuration or compatibility documentation.

## Producer/consumer gate

For each renamed variable, the phase manifest records all literal setters/readers and dynamic-list entries.
The behavioral gate launches the consumer with a nondefault value through the real producer. Directly
setting the new variable in the consumer's unit test is insufficient for cross-process pairs because it
cannot detect a missed setter.

For overrides with fallback defaults, make the old/default location intentionally invalid during the test.
This catches the most dangerous rebrand failure: a half-renamed pair that appears green by silently using a
working default.
