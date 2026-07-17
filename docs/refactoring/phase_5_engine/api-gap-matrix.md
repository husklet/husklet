# Engine API gap matrix

Checked against the current `src/engine/hl-jit` and `src/engine/hl-jit-darwin` public Rust surface. This
compares exposed, composable API—not behavior that happens to be buried inside the old C runtime.

| Capability | Current public API | Gap / replacement API |
| --- | --- | --- |
| Engine discovery | `Runtime::supports(Guest)` | Return versioned host/guest/ISA/Linux/filesystem/network/checkpoint/extension capabilities and limits. |
| Dry validation | none | `Engine::validate(&MachineSpec)` with no host mutation and explicit degraded optional features. |
| Guest selection | `Guest` enum | `GuestPlatform { os_abi, isa, abi, page_size, features }`; artifact selection is separate. |
| Initial process | argv, guest env, cwd, uid/gid | Add umask, groups, capabilities, rlimits as typed values, stdio/PTY, session/group/death-signal behavior. |
| Root filesystem | rootfs plus ordered lowers | Typed root/image/overlay source with upper/work, mount ids, mount options, ownership and coherence. |
| Volumes | host/guest path plus read-only bool | Typed file/directory/socket bind, access mode, ownership mapping, coherence, propagation, executable/nodev/nosuid. |
| Virtual mounts | hardcoded proc/sys/dev/tmp behavior | Configurable procfs/sysfs/devfs/tmpfs/provider mount implementations. |
| Namespace entries | none | Atomic directory/file/symlink/socket/char/block-device projection with complete metadata and backing. |
| Provider files | none | Static, mutable, generated-on-open, host-backed and provider-handle file backing. |
| Provider handles | `render_node: bool` only | Generic open service with read/write/seek/ioctl/mmap/poll/fsync/stat; engine retains fd/OFD semantics. |
| Shared resources | hardcoded IOSurface C path | Opaque provider resource, regions, transferable handles, coherency, sync, fork/exec/checkpoint policy. |
| Guest service IPC | bind-mounted Unix socket | Keep socket mount; add generic framed/shared-ring channel with backpressure, cancellation and transferable handles. |
| Mount hotplug | none | Transactional `Machine::mount/unmount`; capability may report unsupported initially. |
| Network | isolate bool, private key, bridge strings, SOCKS string, port pairs | Typed interfaces, routes, DNS, egress transport, socket policy and forwards; atomic live update. |
| Resource limits | CPU count, memory, pid limit, ulimits | Typed CPU quota/affinity/topology and memory/fd/stack/address-space/I/O/extension budgets with shared accounting. |
| Security | sandbox and network-isolate booleans | Typed authority grants, syscall policy, path/network/provider allowlists and executable-memory policy. |
| Time/entropy | implicit host behavior | Explicit secure versus deterministic entropy and host/offset/frozen/rated guest clocks. |
| Translation cache | directory + ambient switches | Typed store, budget, policy, complete identity, integrity validation and structured metrics. |
| Process control | wait, signal | Add process listing, exec-in-machine, attach, pause guard, shutdown policy and stable process handles. |
| PTY | spawn fd plumbing outside machine model | Typed controlling PTY plus resize, foreground group, hangup and attachment ownership. |
| Events/metrics | logs and ambient debug flags | Bounded structured machine/process/fault/syscall/cache/resource/provider/checkpoint event stream. |
| Debugging | ad-hoc tracing and host signals | Registers, memory, mappings, break/watch points, step, thread control and core dump over authorized control API. |
| Checkpoint | pid + trigger/directory convention | Typed prepare/commit/restore, manifest compatibility and per-extension checkpoint policy. |
| Extension negotiation | none | Versioned manifest/schema/features, required versus optional, prepare/validate and explicit quotas. |
| Lifecycle hooks | hardcoded GPU/fork callbacks | Generic prepare/fork-parent/fork-child/exec/exit/checkpoint callbacks with ordering/deadlines. |
| Host portability | direct macOS calls throughout Linux ABI | Versioned semantic host-services tables; no Linux syscall passthrough and no platform types above backend. |
| Wire compatibility | size/version-prefixed 128-byte header plus string pool | Preserve envelope idea; replace dedicated booleans/ambient dialect with typed, bounded, versioned sections. |

## Existing API to preserve semantically

The following current operations express genuine product needs and should survive as typed parts of
`MachineSpec`, even if names and wire format change:

- root plus ordered image layers;
- bind mounts and read-only root policy;
- argv, environment, cwd, uid/gid and hostname;
- CPU, memory, pid and rlimit controls;
- network isolation, workspace switching, egress routing and published ports;
- filesystem external-writer coherence;
- persistent translation cache;
- stdin/stdout/stderr/PTY attachment;
- wait, signal, checkpoint and restore.

## Current domain leakage to eliminate

- `DeviceRequest::render_node` and `LaunchConfig::gpu_iosurface`;
- engine-owned `/dev/dri`, libdrm sysfs payloads and IOSurface allocation policy;
- ambient `HL_*` switches as a second host control plane;
- checkpoint trigger files/host signal numbers as public control API;
- egress implementation encoded as a SOCKS endpoint instead of a network transport capability;
- a guest enum that also performs host artifact discovery.

## First implementation seam

The first generic seam should be namespace projection plus provider-backed handles. It replaces the narrowest
domain leak while enabling the widest set of consumers:

1. Add versioned `ExtensionSpec` sections to the launch payload.
2. Support atomic projected directories, files, symlinks and character devices.
3. Add provider handle registration for ioctl/mmap/poll.
4. Convert the current render-node model into a GPU-domain provider using those primitives.
5. Run libdrm, Wayland dmabuf, Chrome and Vulkan/Zed tests.
6. Delete `render_node`, `gpu_iosurface` and the engine's hardcoded GPU paths.

Do not start by generalizing the boolean to a string device type. That retains the same engine-owned policy.
