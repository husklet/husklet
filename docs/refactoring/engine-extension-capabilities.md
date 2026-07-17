# Engine extension capabilities

The execution engine must emulate Linux processes without knowing product domains such as GPU, Vulkan,
CUDA, VPN, terminal, or Wayland. Husklet composes domain providers and supplies their requirements to the
engine as capability-scoped configuration. A provider owns Linux-facing policy; the engine owns correct
Linux execution and resource semantics.

The current `gpu_iosurface`/`render_node` boolean is not an adequate contract. It causes the old engine to
contain `/dev/dri`, sysfs, libdrm, IOSurface, and allocation policy. The replacement must accept generic
namespace entries and handle services instead.

## Practical API checklist

This is the minimum concrete API the replacement engine must expose to Husklet and its domain providers.
The later sections explain versioning and ownership; this section is the implementation checklist.

### High-level launch API

```rust
Engine::capabilities() -> Capabilities
Engine::validate(spec: MachineSpec) -> Validation
Engine::spawn(spec: MachineSpec, io: ProcessIo) -> Machine

MachineSpec::guest(os, arch, abi)
MachineSpec::root(source: RootSource)
MachineSpec::overlay(lower_layers, upper, work)
MachineSpec::mount(Mount)
MachineSpec::volume(Volume)
MachineSpec::tmpfs(path, options)
MachineSpec::procfs(path, options)
MachineSpec::sysfs(path, options)
MachineSpec::devfs(path, options)
MachineSpec::process(argv, env, cwd, identity)
MachineSpec::stdio(stdin, stdout, stderr)
MachineSpec::pty(PtySpec)
MachineSpec::network(NetworkSpec)
MachineSpec::resources(ResourceSpec)
MachineSpec::security(SecuritySpec)
MachineSpec::extension(ExtensionSpec)
```

Required mount forms:

```rust
pub enum MountSource {
    HostPath(HostPath),             // file or directory bind
    ImageLayer(ImageLayerHandle),   // immutable content-addressed layer
    Overlay(OverlaySpec),           // ordered lowers + upper + work
    Tmpfs(TmpfsSpec),
    Procfs(ProcfsSpec),
    Sysfs(SysfsSpec),
    Devfs(DevfsSpec),
    Provider(ProviderId),           // extension-owned virtual tree
}

pub struct Mount {
    pub source: MountSource,
    pub target: GuestPath,
    pub read_only: bool,
    pub executable: bool,
    pub nodev: bool,
    pub nosuid: bool,
    pub propagation: Propagation,
}

pub struct Volume {
    pub host: HostPath,
    pub guest: GuestPath,
    pub access: VolumeAccess,
    pub coherence: CoherencePolicy,
    pub ownership: OwnershipMapping,
}
```

Mounts must be ordered, independently read-only/read-write, and able to target a path that does not exist
in the image. File-on-file, directory-on-directory, socket mounts, nested mounts, overlay whiteouts, and
unmount-on-exit are required. The engine returns a mount id so live inspection and checkpoint manifests do
not identify mounts by path alone.

### High-level live API

```rust
Machine::exec(ProcessSpec, ProcessIo) -> Process
Machine::signal(SignalTarget, Signal)
Machine::pause() -> PauseGuard
Machine::shutdown(ShutdownPolicy)
Machine::stats() -> MachineStats
Machine::events() -> EventStream
Machine::mount(Mount) -> MountHandle              // optional hot mount
Machine::unmount(MountHandle, UnmountPolicy)
Machine::update_resources(ResourceUpdate)
Machine::update_network(NetworkUpdate)
Machine::checkpoint(CheckpointSink, CheckpointOptions)

Process::pid() -> GuestPid
Process::wait() -> ExitStatus
Process::signal(Signal)
Process::attach(AttachSpec) -> Attachment
Process::resize_pty(cols, rows)
```

`exec` here means start an additional process in an existing machine/workspace; guest `execve` remains a
normal Linux syscall implemented by the engine.

### Low-level virtual filesystem API

Domain providers must be able to install arbitrary Linux-visible entries without engine changes:

```rust
Namespace::transaction(Vec<NamespaceOperation>) -> NamespaceRevision

NamespaceOperation::mkdir(path, mode, uid, gid)
NamespaceOperation::file(path, FileBacking, metadata)
NamespaceOperation::symlink(path, target, metadata)
NamespaceOperation::char_device(path, DeviceNumber, service, metadata)
NamespaceOperation::block_device(path, DeviceNumber, service, metadata)
NamespaceOperation::socket(path, SocketBacking, metadata)
NamespaceOperation::remove(path)
```

`FileBacking` must support static bytes, mutable shared bytes, a host file, generated-on-open bytes, and a
provider handle. This is how a provider supplies `/sys/.../uevent`, ICD manifests, resolv.conf, device
metadata, secrets, or future kernel-style interfaces. The engine must not hardcode their contents.

One namespace model must answer all of these guest Linux operations consistently:

- `open`, `openat`, `openat2`, `creat`, `close`, `close_range`;
- `stat`, `lstat`, `fstat`, `newfstatat`, `statx`, `access`, `faccessat`;
- `readlink`, `readlinkat`, `realpath` behavior;
- `getdents64`, directory seek/cookies, `chdir`, `fchdir`, `getcwd`;
- `mkdir`, `mknod`, `link`, `symlink`, `rename`, `unlink`, `rmdir`;
- `chmod`, `chown`, `utimensat`, umask, xattrs;
- file and OFD locks, leases where supported;
- `read`, `write`, `pread`, `pwrite`, vectored I/O, splice/sendfile where supported;
- `truncate`, `fallocate`, sparse-file semantics, `fsync`/`fdatasync`;
- `mmap` coherence and invalidation after external host writes;
- inotify events for changes visible to the guest.

The important requirement is not that every provider implements these operations. The engine supplies
correct default behavior and rejects unsupported meaningful operations with the correct errno. A provider
only implements operations intrinsic to its entry.

### Low-level provider handle API

Opening a provider-backed node returns an open file description. The engine needs this interface:

```rust
trait OpenHandle {
    read(offset, buffers, flags) -> Completion<usize>;
    write(offset, buffers, flags) -> Completion<usize>;
    seek(offset, whence) -> Result<u64, LinuxError>;
    ioctl(command, input, output_len) -> Completion<IoctlReply>;
    mmap(request: MapRequest) -> Result<Mapping, LinuxError>;
    poll(interest: PollInterest) -> PollSubscription;
    fsync(data_only: bool) -> Completion<()>;
    stat() -> Result<Stat, LinuxError>;
    set_status_flags(flags) -> Result<(), LinuxError>;
    close();
}
```

The engine, not the provider, owns guest fd numbers and implements:

- `dup`, `dup2`, `dup3`, `fcntl(F_DUPFD*)`;
- shared open-file-description offsets and status flags;
- per-descriptor `CLOEXEC`;
- fork inheritance and exec cleanup;
- `close_range`, fd reuse, and process-exit cleanup;
- interruption, restart, cancellation, and async completion races.

For `ioctl`, the engine validates guest pointers from the ioctl direction/size bits, copies input into owned
bytes, invokes the provider, and copies bounded output back. A provider never receives a raw guest pointer.

### Low-level memory API

```rust
Memory::reserve(AddressRequest) -> Reservation
Memory::map(MappingSource, AddressRequest, Protection, MapFlags) -> Mapping
Memory::unmap(range)
Memory::protect(range, protection)
Memory::advise(range, advice)
Memory::share(range, SharingPolicy) -> SharedRegion
Memory::dirty_pages(range) -> Bitmap
Memory::invalidate(range)
```

`MappingSource` supports anonymous memory, files, provider resources, transferred handles, and shared
regions. The engine must implement Linux `mmap`, `mremap`, `munmap`, `mprotect`, `msync`, `madvise`,
`mincore`, `mlock`, shared versus private/COW mappings, guard pages, and executable mappings.

Provider mappings declare:

- length and alignment;
- allowed protections;
- shared/COW behavior across guest fork;
- CPU visibility and coherency;
- whether the mapping survives exec;
- checkpoint strategy;
- an optional readiness/synchronization object.

This supports GPU buffers, shared command rings, file mappings, JIT runtimes inside the guest, databases,
language runtimes, and future accelerators without a GPU-specific engine type.

### Low-level IPC and descriptor transfer API

The guest Linux ABI must support:

- Unix stream, datagram, and seqpacket sockets;
- pathname and abstract Unix addresses;
- nonblocking connect/accept/read/write;
- `poll`, `ppoll`, `select`, `epoll`, edge/level/oneshot behavior;
- `sendmsg`/`recvmsg` with `SCM_RIGHTS` and credentials;
- eventfd, timerfd, signalfd, pipes, socketpair, and memfd;
- shutdown, peer close, half-close, and cancellation semantics.

The engine API needs transferable `EngineHandle` objects. A provider can attach an engine-owned handle to a
message; the engine allocates a guest fd on receive. Raw host fd numbers and guest fd numbers never cross
the provider boundary.

Wayland requires Unix sockets, polling, shared-memory fds, and `SCM_RIGHTS`. GPU buffer protocols require
the same fd transfer plus provider mappings. Terminal and language-server workloads heavily exercise
epoll, pipes, socketpairs, signals, and process lifecycle.

### Low-level process and thread API

The engine core must implement these Linux behaviors rather than delegate them to extensions:

- `fork`, `vfork`, `clone`, `clone3`, pthread-style thread creation;
- `execve`/`execveat`, ELF loading, interpreters, auxv, TLS, and `CLOEXEC`;
- `exit`, `exit_group`, `wait*`, parent death, zombies, reparenting;
- process groups, sessions, controlling terminals, job-control signals;
- credentials, groups, capabilities, `setuid`/`setgid` family;
- all standard signals, masks, alternate stacks, queued RT signals, restart semantics;
- futexes, robust lists, rseq policy, atomics, TLS, thread names;
- scheduling, affinity, priorities, CPU topology reporting;
- timers, sleeps, clock syscalls, timerfd, CPU clocks;
- `/proc` views consistent with actual processes, fds, maps, mounts, and limits.

The control API exposes spawn/exec, signal, wait, pause/resume, inspection, and PTY resize. It does not
expose internal host pids as stable identities.

### Low-level network transport API

```rust
trait NetworkTransport {
    socket(domain, type, protocol) -> NetworkHandle;
    resolve_route(flow) -> RouteDecision;
    connect(handle, address) -> Completion<()>;
    bind(handle, address) -> Result<()>;
    listen(handle, backlog) -> Result<()>;
    accept(handle) -> Completion<NetworkHandle>;
    send(handle, message) -> Completion<usize>;
    recv(handle, request) -> Completion<Message>;
}
```

The engine supplies Linux socket state and syscalls; transports supply host/NAT, virtual-switch, VPN,
proxy, packet, or test-loopback behavior. Route and egress policy can be replaced atomically at runtime.
The API must cover TCP, UDP, IPv4/IPv6, Unix sockets, socket options, DNS configuration, netlink views,
interface/address/route reporting, port forwarding, and network isolation.

### Host and guest kernel requirements

There are two different kernels involved:

1. The **guest Linux kernel ABI is emulated**. There is no guest kernel to configure. If Chrome or Zed
   expects a Linux facility, the engine must implement its observable syscall/filesystem/device behavior or
   a provider must supply it through the generic APIs above.
2. The **host kernel** supplies primitives the engine uses. Requirements differ by host platform and must
   be reported by `Engine::capabilities()`.

For the current macOS host implementation the engine needs, at minimum:

- executable-memory/JIT entitlement and supported W^X mechanism;
- virtual-memory reserve/map/protect/remap/inheritance operations;
- host threads, TLS, atomics, signals/exceptions, and monotonic clocks;
- kqueue or equivalent readiness notification;
- Unix sockets with descriptor passing;
- PTYs and ordinary file descriptors;
- process spawn and whatever fork behavior the implementation elects to use;
- sandbox/file/network authority explicitly granted to the signed application;
- Mach ports/shared-memory handles when a provider chooses them;
- IOSurface/Metal only for the macOS GPU provider, never as an engine-core requirement.

On a Linux host, likely primitives are `mmap`/`mprotect`, epoll, eventfd, timerfd, memfd, Unix sockets,
`SCM_RIGHTS`, PTYs, userfaultfd/seccomp only if the chosen implementation uses them, and KVM only for an
optional virtualization backend. The public API must not assume KVM, DRM, CUDA, or a particular host GPU.

### Linux facility matrix for Husklet

| Product/domain | Linux-visible facilities the engine must make possible |
| --- | --- |
| Workspace shell | ELF/interpreter loading, PTY, signals, process groups, fork/exec/wait, procfs |
| Images/rootfs | overlay semantics, mounts, whiteouts, permissions, xattrs, mmap, external-write coherence |
| Containers | pid/uts/user/network/mount isolation, uid/gid, limits, hostname, ports, `/proc`, `/sys`, `/dev` |
| VPN | sockets, routes, DNS, interface views, atomically replaceable egress transport |
| Wayland | Unix socket, nonblocking I/O, poll/epoll, memfd/shm, `SCM_RIGHTS` |
| GL/EGL | injected libraries, loader paths, threads/TLS, Unix IPC, shared mappings, transferable handles |
| Vulkan/Zed | ICD manifest/library, loader discovery, futexes, epoll, shared mappings, handle transfer, sync |
| CUDA/ML | libraries, large mappings, threads, atomics, shared command channel, resource limits |
| GPU allocation | configurable `/dev` node, coherent sysfs entries/symlinks, ioctl, mmap, poll/sync, fork policy |
| Browser | all above plus sandbox syscalls, close-range, OFD locks, shared memory, Mojo fd passing, timers |
| Databases | file/OFD locks, fsync, mmap coherence, fallocate, robust futexes, sockets, clocks |
| Language tools | fork/exec, pipes/socketpairs, epoll, inotify, signals, mmap/JIT permissions |
| Checkpoint | complete process/thread/memory/fd/timer/socket/provider state or explicit reconnect policy |

### What must remain configurable instead of hardcoded

- every guest path and mount target;
- file bytes, symlink targets, modes, uid/gid, timestamps, and device major/minor;
- directory contents and stable directory cookies;
- ioctl command families and their bounded request schemas;
- provider mappings, inheritance, coherency, and checkpoint behavior;
- guest libraries, executables, loader manifests, environment, and service endpoints;
- network interfaces, addresses, routes, DNS, egress, and published ports;
- CPU model/features/topology, identity, hostname, limits, time, and entropy policy;
- provider identifiers and versioned configuration.

The engine may hardcode Linux ABI rules—what `dup` means, how signal masks work, how a shared mapping
behaves—but must not hardcode product device names, sysfs payloads, GPU brands, VPN types, workspace
settings, or host backend identities.

## Required engine capabilities

### Guest namespace projection

The engine must be able to project these entries at arbitrary absolute guest paths:

- directories with mode, uid, gid, and deterministic enumeration;
- regular files backed by immutable bytes, mutable shared bytes, or a host file;
- symbolic links with a supplied target;
- bind-mounted host files, directories, and Unix sockets;
- character and block nodes with configurable Linux `dev_t` and metadata;
- generated files whose bytes are supplied by a provider when opened or read.

All path operations must agree: `openat`, `stat`/`statx`, `readlinkat`, directory iteration, access checks,
canonicalization, and `/proc/self/fd` views must observe one coherent namespace. Providers must not patch
individual syscalls independently.

This capability is enough to describe `/dev/dri`, `/sys/dev/char`, Vulkan ICD manifests, injected shared
libraries, CUDA tools, Wayland sockets, resolv/hosts configuration, and future devices without adding
their names to the engine.

### Open-handle services

A projected node may name an open service. Opening it creates an opaque engine handle whose provider can
implement only the operations it supports:

- read, write, positioned I/O, seek, truncate, and metadata updates;
- typed ioctl request/response byte buffers;
- `mmap`/`munmap` and protection changes;
- `poll`/`epoll` readiness and wakeups;
- descriptor export/import for `SCM_RIGHTS`;
- close and resource release.

The engine remains responsible for Linux descriptor allocation, duplication, `CLOEXEC`, fork inheritance,
close-range behavior, cancellation, and translating provider errors into Linux errno values. A provider
must never receive or manage guest fd numbers.

### Shared memory and host resources

Providers need a generic allocation/import contract that can return:

- a guest-mappable memory region with size, alignment, protections, and sharing mode;
- an opaque provider resource id;
- an optional engine handle suitable for fd passing;
- explicit CPU/GPU synchronization and completion signals;
- declared fork and exec inheritance behavior.

The engine must support shared mappings across emulated fork, not assume every host resource is safe to
create after a host fork, and expose lifecycle callbacks so a provider can preallocate, retain, duplicate,
or invalidate resources. IOSurface is one implementation in a macOS GPU provider; it is not an engine API
type.

### IPC endpoints

The engine must project host Unix sockets and preserve Linux socket behavior needed by Wayland and domain
services, including ancillary fd passing, credentials where supported, nonblocking I/O, polling, shutdown,
and peer-close semantics. A provider may alternatively expose an engine-managed request channel when a
host object cannot be represented by a host fd.

### Process lifecycle

Capability providers need narrow notifications for:

- process creation before the first guest instruction;
- fork prepare, parent completion, and child completion;
- exec, including `CLOEXEC` cleanup;
- thread and process exit;
- checkpoint/restore if enabled.

Notifications carry opaque process/resource identities, not product service locators. The engine defines
ordering and guarantees that callbacks cannot observe partially-mutated fd or address-space state.

### Launch configuration and negotiation

The typed launch API must carry a versioned collection of extension specifications rather than dedicated
booleans. Each specification contains:

- a stable capability/provider identifier and contract version;
- namespace projections;
- open-service endpoints;
- host resource/memory requirements;
- guest environment additions;
- required versus optional status and feature bits.

The engine validates the complete specification before launch and returns supported contract versions and
features. Unknown required capabilities fail explicitly; unknown optional capabilities are reported as
unavailable. Limits must bound paths, metadata, request sizes, mappings, handles, and queued events.

### Security and isolation

Every projected entry and service is scoped to one launch. The engine must enforce declared access modes,
prevent path escape, validate all guest pointers and lengths before provider calls, bound allocations and
queues, and revoke resources on process exit. Providers receive the least authority required for their
capability.

## Domain mapping

| Consumer | Engine capabilities it needs | Domain-owned behavior |
| --- | --- | --- |
| GL/EGL | library/file projection, Unix socket IPC, shared mappings, fd passing | EGL/GLES ABI, IR lowering, buffer policy |
| Vulkan | ICD/library projection, Unix socket IPC, shared mappings, fd passing | Vulkan ABI, memory model, IR lowering |
| CUDA/NVML | library/tool projection, service IPC, shared mappings | CUDA/NVML ABI, compute/device model |
| GPU allocation | namespace nodes, generated sysfs files/symlinks, ioctl handles, mmap, fork lifecycle | DRM-visible model, host allocator, resource ids |
| Wayland/compositor | Unix socket projection, fd passing, polling | protocols, surface state, buffer import, presentation |
| Terminal | PTY handles, polling, signals, resize ioctl | terminal session and UI composition |
| VPN/networking | socket/network policy endpoint and launch configuration | VPN selection, credentials, routes, workspace policy |
| Workspace storage | mounts, generated files, coherence notification | image/workspace layout and persistence policy |

For the current Chrome failure, a GPU provider would describe a platform device namespace, its generated
metadata, and an ioctl/mmap service. Values such as `OF_COMPATIBLE_0=husklet,gpu` belong in that provider's
Linux-facing device model. The engine should only guarantee that the configured file is visible and
consistent through every relevant filesystem operation.

## API shape

The replacement should expose several cohesive ports rather than one application-wide trait:

- `Namespace` installs validated entries;
- `Handles` opens provider-backed resources;
- `Memory` maps and shares regions;
- `Events` delivers readiness/completion;
- `Lifecycle` defines fork/exec/exit behavior;
- `Extensions` negotiates versioned provider specifications.

Husklet gathers specifications from GPU, surface, terminal, networking, and workspace providers and passes
the resulting launch configuration to the engine. No domain crate depends on Husklet, and the engine does
not branch on a domain name.

## Complete public API surface

The engine API has three planes. Keeping them separate prevents launch configuration, live control, and
guest fast paths from collapsing into one unversioned bag of options.

### Discovery plane

```rust
pub trait Engine {
    fn capabilities(&self) -> EngineCapabilities;
    fn validate(&self, spec: &MachineSpec) -> Result<Validation, SpecError>;
    fn spawn(&self, spec: MachineSpec, io: ProcessIo) -> Result<Machine, SpawnError>;
}

pub struct EngineCapabilities {
    pub api: Version,
    pub guests: Vec<GuestPlatform>,
    pub cpu: CpuCapabilities,
    pub linux: LinuxCapabilities,
    pub filesystems: FilesystemCapabilities,
    pub networking: NetworkCapabilities,
    pub checkpoint: CheckpointCapabilities,
    pub extensions: Vec<ExtensionCapability>,
    pub limits: EngineLimits,
}
```

Discovery must report guest OS/architecture combinations, ISA extensions, page sizes, syscall ABI level,
filesystem and network modes, checkpoint compatibility, extension contract versions, and hard limits. It
must not merely return a boolean such as `supports(aarch64)`.

`validate` performs the same complete validation as `spawn` without changing host state. Its result
contains selected versions, degraded optional features, computed namespace conflicts, and estimated host
resources. This lets Husklet explain an invalid workspace before opening a terminal.

### Launch plane

```rust
pub struct MachineSpec {
    pub guest: GuestPlatform,
    pub cpu: CpuSpec,
    pub process: ProcessSpec,
    pub identity: IdentitySpec,
    pub filesystem: FilesystemSpec,
    pub namespaces: NamespaceSpec,
    pub network: NetworkSpec,
    pub resources: ResourceSpec,
    pub security: SecuritySpec,
    pub time: TimeSpec,
    pub entropy: EntropySpec,
    pub cache: TranslationCacheSpec,
    pub checkpoint: CheckpointSpec,
    pub observability: ObservabilitySpec,
    pub extensions: Vec<ExtensionSpec>,
}
```

No field is encoded through an ambient `HL_*` variable. Environment variables belong only in
`ProcessSpec.env`, where they become guest process data. Host paths, secrets, service endpoints, debug
switches, and engine policy remain typed launch fields.

#### Guest and CPU

`GuestPlatform` contains OS ABI, architecture, endianness, ABI variant, page size, and minimum kernel ABI.
`CpuSpec` selects visible topology, feature bits, model strings, frequency reporting, affinity, and whether
unsupported instructions trap, emulate, or fail validation. CPUID/auxv/procfs answers derive from this one
model so applications cannot observe contradictory CPU descriptions.

The engine owns instruction translation, signal-safe fault recovery, self-modifying-code invalidation,
atomic and memory-order semantics, executable-memory policy, and translation-cache correctness. These are
not extension hooks.

#### Initial process

`ProcessSpec` contains argv, ordered environment, cwd, executable selection, umask, controlling terminal,
stdio endpoints, extra inherited handles, rlimits, death signal, and session/process-group behavior.
Empty argv, missing cwd, invalid environment keys, and unsupported meaningful options fail validation.

`ProcessIo` accepts inherited host handles or engine channels independently for stdin, stdout, stderr, and
the controlling PTY. A PTY API exposes resize, foreground process group, hangup, packet mode, and window
size; terminal code must not reach into engine internals.

#### Identity and Linux namespaces

`IdentitySpec` defines uid, gid, supplementary groups, hostname/domain name, securebits, capabilities,
credentials visible through procfs, and configurable passwd/group projection if desired.

`NamespaceSpec` independently selects mount, pid, uts, ipc, network, user, and cgroup isolation modes.
Sharing is expressed with an opaque namespace handle, not a string key. PID allocation, parent/child
relationships, sessions, process groups, and signal permissions are engine-owned Linux semantics.

#### Filesystems

`FilesystemSpec` contains the root tree, ordered overlay layers, writable upper/work storage, read-only
policy, mount propagation, initial mounts, generated standard files, and cache/coherence policy.

```rust
pub enum TreeSource {
    HostDirectory(HostPath),
    ImageLayer(ImageLayerHandle),
    Overlay { lower: Vec<TreeSource>, upper: HostPath, work: HostPath },
    Provider(ProviderId),
}

pub enum NamespaceEntry {
    Directory(DirectoryEntry),
    File(FileEntry),
    Symlink(SymlinkEntry),
    Device(DeviceEntry),
    HostBind(HostBindEntry),
    Socket(SocketEntry),
}
```

The VFS contract covers path resolution, symlink limits, permissions, hard links, rename/unlink, locks,
xattrs, timestamps, sparse files, mmap coherence, inotify/fanotify where supported, statfs, and directory
cookies. External host writes enter through an explicit coherence API carrying changed identities or a
generation; a magic generation filename is an implementation, not the public contract.

Standard virtual trees (`procfs`, `sysfs`, `devtmpfs`, `tmpfs`, `cgroupfs`) are mount implementations with
typed configuration. Extensions add entries through `Namespace`, but cannot independently forge answers
for `stat`, `readlink`, and `open`.

#### Networking

```rust
pub struct NetworkSpec {
    pub namespace: NetworkNamespace,
    pub interfaces: Vec<InterfaceSpec>,
    pub routes: Vec<RouteSpec>,
    pub dns: DnsSpec,
    pub egress: EgressPolicy,
    pub listeners: Vec<PortForward>,
    pub socket_policy: SocketPolicy,
}
```

Required modes include no network, loopback only, host/NAT egress, virtual switches between workspaces,
explicit port publishing, and provider-backed egress such as VPN/SOCKS. Interfaces, addresses, routes,
DNS, `/etc/hosts`, socket names, and getsockname/getpeername results must derive from the same model.

The live control API can atomically replace egress policy and routes. Husklet therefore handles a VPN
settings change by updating `NetworkControl`; neither GUI nor engine learns a VPN product type.

The engine owns Linux socket semantics, nonblocking behavior, polling, ancillary data, cancellation, and
fd lifecycle. A network provider owns packet/connection transport and policy.

#### Resources and scheduling

`ResourceSpec` covers memory reservation/limit, process/thread limit, CPU count/quota/affinity, open files,
file size, locked memory, stack, address space, I/O limits, and extension-specific allocation budgets.
Usage is observable through one accounting model used by the control API, procfs, and cgroupfs.

The scheduler must provide correct futexes, robust lists, thread-local storage, clone flags, priorities,
affinity, timers, and interruption/restart behavior. Optional deterministic scheduling is a declared mode,
primarily for tests and replay.

#### Time and entropy

`TimeSpec` selects host real time, offset/frozen virtual real time, monotonic origin, timer resolution, and
rate. Every clock syscall, vDSO substitute, timerfd, timeout, and procfs report uses it.

`EntropySpec` selects secure host entropy or a deterministic seeded source. Secure mode must never silently
fall back to deterministic bytes. Deterministic mode is useful for reproducible tests and snapshots.

#### Security

`SecuritySpec` selects trusted versus untrusted guest mode, syscall policy, filesystem authority, network
authority, host resource grants, executable-memory rules, and provider allowlists. Policies compile before
launch; a rejected syscall returns the configured Linux failure or terminates the process observably.

Host paths and handles are capabilities granted by the caller. Guest strings can never name arbitrary host
resources. Provider processes should be isolatable out of process; a malformed provider reply must not
corrupt engine memory.

#### Translation cache

`TranslationCacheSpec` supplies a cache store capability, size/budget, identity key inputs, and policy. The
engine defines and versions the cache format, includes all code-generation-affecting inputs in its key,
validates content before execution, and reports hit/miss/invalidation events. Callers do not toggle cache
behavior with ambient environment variables.

#### Checkpoint and restore

Checkpointing is a first-class protocol:

```rust
pub trait CheckpointControl {
    fn prepare(&self, options: CheckpointOptions) -> Result<CheckpointTicket, CheckpointError>;
    fn commit(&self, ticket: CheckpointTicket, sink: &dyn CheckpointSink) -> Result<Checkpoint, CheckpointError>;
    fn restore(&self, source: &dyn CheckpointSource, options: RestoreOptions) -> Result<Machine, RestoreError>;
}
```

The checkpoint manifest versions CPU state, mappings, processes/threads, fd descriptions, mounts, sockets,
timers, signals, provider resources, and engine build compatibility. Every extension declares resources as
checkpointable, externally reconnectable, discardable, or blocking. The engine must refuse a checkpoint
that would silently lose a required resource.

### Live control plane

```rust
pub trait Machine {
    fn id(&self) -> MachineId;
    fn processes(&self) -> Result<Vec<ProcessInfo>, ControlError>;
    fn signal(&self, target: SignalTarget, signal: Signal) -> Result<(), ControlError>;
    fn pause(&self) -> Result<PauseGuard, ControlError>;
    fn update_resources(&self, update: ResourceUpdate) -> Result<(), ControlError>;
    fn update_network(&self, update: NetworkUpdate) -> Result<(), ControlError>;
    fn attach(&self, request: AttachRequest) -> Result<Attachment, ControlError>;
    fn hotplug(&self, extension: ExtensionSpec) -> Result<ExtensionHandle, ControlError>;
    fn events(&self) -> EventStream;
    fn wait(&self) -> Result<ExitStatus, ControlError>;
    fn shutdown(&self, policy: ShutdownPolicy) -> Result<(), ControlError>;
}
```

The control channel is usable from another host process and survives the original CLI. Operations carry
request ids, deadlines, cancellation, and idempotency rules. Pause is reference-counted through a guard so
debugging, checkpointing, and inspection cannot accidentally resume each other's stop.

Attachments cover PTYs, stdio, logs, debugging, and file transfer without giving callers raw engine
internal fds. Hotplug is supported only for extension contracts that declare it; removal has quiesce and
force policies.

### Observability and debugging

`ObservabilitySpec` selects structured events and bounded metrics, not printf flags. The event stream needs:

- machine/process/thread lifecycle and exit reasons;
- exec and image identity;
- signals and fatal guest faults with guest PC/registers;
- syscall latency/error sampling with privacy controls;
- translation/cache statistics and invalidations;
- memory, fd, filesystem, network, and provider resource usage;
- extension negotiation, failure, timeout, and revocation;
- checkpoint progress and restore compatibility failures.

Debug APIs expose register read/write, virtual memory read/write, mappings, breakpoints, watchpoints,
single-step, thread stop/resume, core-dump generation, and guest-to-host translated-PC diagnostics. They are
authorization-gated and do not require recompiling the engine with debug logging.

Tracing uses stable schemas, timestamps from both host monotonic time and guest virtual time, correlation
ids, and explicit loss counters. Production defaults remain bounded.

## Extension contracts

An extension is not an arbitrary callback into engine memory. It is a negotiated collection of the narrow
ports below:

```rust
pub trait ExtensionProvider: Send + Sync {
    fn manifest(&self) -> ExtensionManifest;
    fn prepare(&self, context: PrepareContext, config: Value) -> Result<PreparedExtension, ExtensionError>;
}

pub struct PreparedExtension {
    pub namespace: Vec<NamespaceEntry>,
    pub services: Vec<ServiceRegistration>,
    pub memory: Vec<MemoryRequirement>,
    pub lifecycle: Option<Box<dyn Lifecycle>>,
    pub checkpoint: Option<Box<dyn ExtensionCheckpoint>>,
}
```

`Value` above is validated against the manifest's versioned schema before `prepare`; production APIs may
use generated strongly typed wrappers. It is not passed into the engine as an unexamined JSON blob.

### Namespace port

Installs a transaction of entries. Conflicts, parents, symlink targets, modes, ids, and limits are
validated together. Installation either commits completely or changes nothing. Live updates use another
transaction and produce namespace change events.

### Handle port

```rust
pub trait HandleService {
    fn open(&self, request: OpenRequest) -> Result<Box<dyn OpenHandle>, LinuxError>;
}

pub trait OpenHandle {
    fn read(&self, request: ReadRequest) -> IoFuture;
    fn write(&self, request: WriteRequest) -> IoFuture;
    fn ioctl(&self, request: IoctlRequest) -> IoctlFuture;
    fn map(&self, request: MapRequest) -> Result<Mapping, LinuxError>;
    fn readiness(&self, interest: Interest) -> ReadinessSubscription;
    fn flush(&self) -> IoFuture;
    fn close(self: Box<Self>);
}
```

Requests carry copied/validated data, credentials, cancellation, and deadlines. The engine translates
between guest pointers/iovecs and these owned values. `IoctlRequest` includes direction and bounded input
bytes; its reply includes output bytes and Linux result. Providers never dereference guest memory.

Open-file-description state is distinct from descriptor state so dup/fork semantics remain correct.

### Memory port

```rust
pub trait MemoryProvider {
    fn allocate(&self, request: AllocationRequest) -> Result<HostResource, ResourceError>;
    fn import(&self, descriptor: ResourceDescriptor) -> Result<HostResource, ResourceError>;
}

pub struct HostResource {
    pub id: ResourceId,
    pub regions: Vec<Region>,
    pub handles: Vec<TransferHandle>,
    pub coherency: Coherency,
    pub inheritance: Inheritance,
    pub synchronization: Synchronization,
}
```

Regions declare size, alignment, allowed protections, sharing/COW behavior, CPU visibility, and dirty-page
tracking. Transfer handles are engine-owned capabilities suitable for guest fd passing. Coherency states
whether explicit flush/invalidate is required. Synchronization exposes wait/signal objects without naming
Metal, Vulkan, CUDA, or DRM.

### Guest service channels

Injected guest libraries frequently need a fast command path. The engine should offer a generic channel
facility with framed messages, shared rings, bulk shared-memory regions, transferable handles, readiness,
backpressure, cancellation, and peer death. Providers choose Unix-socket compatibility or an optimized
engine channel; both expose equivalent lifecycle semantics.

There is no global GPU hypercall. GL, Vulkan, CUDA, language accelerators, audio, USB, secret agents, and
future services can each define their own versioned message protocol over a channel.

### Lifecycle port

Lifecycle callbacks are ordered around engine state transitions and receive opaque ids plus declared
resources. Fork preparation can veto or request preallocation. Child completion can remap/duplicate only
resources whose inheritance policy allows it. Exec receives the final surviving open descriptions after
`CLOEXEC`. Exit is guaranteed exactly once. Callbacks have deadlines and cannot call back synchronously
into arbitrary machine control operations.

### Standard Linux personality extensions

The engine core implements ordinary Linux syscalls. For legitimate nonstandard ABI needs, a separately
versioned personality port may register:

- architecture-specific auxv entries;
- prctl/arch_prctl operations in an allocated command namespace;
- ioctl families attached to provider handles;
- netlink families or socket protocols attached to a provider;
- virtual filesystem types;
- optional instruction/copressor traps in an allocated encoding range.

It must not offer an unscoped "intercept every syscall and mutate registers" production API. That would
make correctness, security, checkpointing, and compatibility impossible to reason about. A debug-only
syscall observer can inspect or fault-inject under explicit unsafe authorization.

## Errors, compatibility, and ownership

All APIs return typed errors with a stable category, human context, offending field/resource, and optional
Linux errno. Provider crashes, protocol mismatch, timeout, cancellation, quota exhaustion, guest fault,
host failure, and unsupported configuration are distinct.

Wire protocols use a frozen envelope containing magic, protocol version, message type, length, request id,
and feature bits. Unknown required fields fail. Tail additions and unknown optional messages are safely
skipped. Every persisted checkpoint/cache/provider descriptor records its contract and implementation
versions.

Ownership rules are explicit:

- Husklet owns product policy and composition.
- Domain crates own device/service models and guest protocols.
- The engine owns Linux semantics, guest memory, process state, and descriptor tables.
- Providers own host resources behind opaque ids.
- The control client owns only opaque machine/process/attachment handles.

## Requirement matrix for this repository

| Product behavior | Launch APIs | Live APIs | Extension APIs |
| --- | --- | --- | --- |
| Open reproducible workspace | guest, process, identity, filesystem, namespaces, resources | wait, signal, usage, checkpoint | image tree/coherence provider |
| Interactive terminal | process PTY/stdio | attach, PTY resize, signal, foreground group | optional terminal integration channel |
| Set or change VPN | initial network/egress policy | atomic route/egress update | connection/packet transport provider |
| Run Docker-compatible workloads | overlay root, mounts, env, user, limits, network, ports | exec/attach, stats, file transfer | image/coherence providers |
| Wayland application | socket projection, fd passing | lifecycle and surface-service health | compositor channel/socket provider |
| Chrome GL | library projection, device namespace, shared resources | GPU service/resource events | GL protocol, allocation handle, dmabuf model |
| Zed Vulkan | ICD projection, device namespace, shared resources | GPU service/resource events | Vulkan protocol, allocation/sync handles |
| CUDA/ML | library/tool projection, memory budget | accelerator usage and cancellation | CUDA/NVML protocol and memory provider |
| Host presentation | no engine-specific product flag | surface attach/detach events if needed | transferable resource descriptors |
| Suspend/resume workspace | checkpoint policy | pause/checkpoint/restore | extension checkpoint adapters |
| Tests and performance work | deterministic time/entropy/scheduling, observability | fault injection, metrics, trace | mock providers and loopback channels |

## Migration rule

For every old-engine special case, migration follows the same testable sequence:

1. Record the observable Linux behavior and consumers.
2. Classify it as core Linux semantics, launch policy, or an extension capability.
3. Add or use the narrow generic engine port.
4. Implement the domain behavior outside the engine.
5. Run compatibility tests against real consumers.
6. Remove the old environment switch, hardcoded path, and syscall branch.

Examples:

- `gpu_iosurface` becomes namespace entries + handle/memory/lifecycle services supplied by the GPU domain.
- `egress_socks` becomes an `EgressPolicy` backed by a network transport provider.
- `fsgen_file` becomes a filesystem coherence channel.
- ambient checkpoint directories become `CheckpointSource`/`CheckpointSink` capabilities.
- debug/performance environment variables become bounded `ObservabilitySpec` and debug-control requests.
