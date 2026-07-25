# Engine capabilities

## Why Husklet needs the engine

Husklet is a macOS application for persistent Linux workspaces without a VM. A user selects an OCI image,
opens a workspace, and receives a Linux terminal. The workspace may also contain host directories, resource
limits, a VPN policy, Docker-compatible container services, and graphical Linux applications such as Chrome,
GTK applications, or Zed.

Guest programs are ordinary Linux programs and should observe ordinary Linux mechanisms:

```text
Husklet workspace configuration
  -> container image, overlay, mounts, identity, network, resources
  -> engine Linux machine/domain
  -> guest processes and PTYs
  -> projected sockets, files, libraries, and devices
  -> host services selected by Husklet
```

For graphics, guest Wayland clients connect to a projected Unix socket. Guest GL, Vulkan, and CUDA libraries
translate calls into a neutral GPU protocol. A host GPU service executes that protocol through WebGPU, and a
compositor presents Wayland surfaces as native macOS windows. A workspace may similarly project a Docker
socket or route network operations through a host-selected VPN transport.

Husklet owns product policy: which image to use, which features a workspace enables, how settings appear,
which host implementation is selected, and which authority that implementation receives. Containers own
container lifecycle, image/storage composition, and Docker semantics. The engine owns the reusable execution
mechanisms that make a declared Linux environment real.

## The engine is a toolbox, not a Husklet backend

The engine must survive uses that have nothing to do with Husklet: CI sandboxes, language runners, IDEs,
serverless workers, compatibility layers, deterministic tests, device-forwarding tools, container products,
and applications not yet imagined. Therefore every addition must describe a general execution capability,
not a product, protocol, device brand, or current workaround.

The correct response to a missing capability is to extend the engine's generic vocabulary and lowering—not
to encode policy in environment variables, hardcode one device, or make Husklet mount around the API.

| Husklet need | Engine primitive | Forbidden engine concept |
| --- | --- | --- |
| Wayland connection | Projected Unix socket/endpoint | `WaylandSocket` |
| GPU render node | Provider-backed character device, handles, memory, events | `HuskletGpu` or `MetalDevice` |
| GL/Vulkan/CUDA libraries | Declarative namespace files and read-only projections | CUDA-specific library injection |
| Docker access | Projected Unix socket with Linux socket semantics | `DockerSocket` |
| VPN | Routes, DNS policy, and an authorized network transport | `VpnConfig`, `Socks5Mode`, or a VPN brand |
| Workspace shell | Machine domain, process execution, PTY, signals | `WorkspaceTerminal` |
| Native UI status | Typed lifecycle/resource/fault events | Husklet callbacks or stderr parsing |

An engine primitive is sufficiently generic when:

- its names make sense without knowing Husklet exists;
- at least several unrelated systems could use it unchanged;
- policy and secrets remain with the caller while mechanism and Linux semantics remain with the engine;
- the backend can discover, validate, lower, observe, and clean it up through one typed contract;
- unsupported behavior fails explicitly before launch instead of becoming a silent approximation;
- adding another provider or backend does not require modifying engine core dispatch for its domain name.

Generic does not mean one untyped escape hatch. Byte blobs, arbitrary callbacks, string maps, shell commands,
and ambient environment variables merely move the coupling out of sight. The API should expose small typed
atoms—paths, namespace entries, handles, mappings, endpoints, routes, credentials, quotas, events, processes,
and lifecycles—that callers compose. Extension configuration may carry provider-owned versioned bytes, but
the engine still validates its schema, authority, resource bounds, and interaction with the machine.

## Responsibility boundary

The engine provides:

- a declarative description of a Linux machine and its process domain;
- capability discovery and complete preflight validation;
- filesystems, namespace nodes, mounts, sockets, devices, networking, identity, resources, security, and PTYs;
- provider registration for mechanisms implemented outside engine core;
- explicit launch-scoped authority for host resources;
- process/domain control, structured observation, and deterministic cleanup;
- backend-independent contracts with backend-specific lowering hidden behind them.

The engine does not provide:

- workspace, settings-screen, image-selection, or application policy;
- GPU, Docker, Wayland, Chrome, VPN-brand, or macOS product concepts;
- host-service discovery by executable name or hardcoded application paths;
- implicit authority from environment variables, global state, or guessed host resources;
- claims of compatibility that its active backend has not passed in conformance tests.

Husklet must be able to express its entire workspace using these tools. If it cannot, modify the engine API
or implementation. Do not preserve an inadequate API by moving engine work into Husklet or containers.

## Contract

- `EngineCapabilities` reports only behavior implemented by the active backend.
- `Engine::validate` performs complete, side-effect-free preflight with field-addressed errors.
- `MachineSpec` is declarative, cloneable, inspectable, and contains no host authority or secrets.
- Host authority is granted separately at spawn and scoped to one machine/domain.
- Optional features may degrade explicitly; required features fail before the guest starts.
- No public behavior enters through ambient `HL_*` variables, hardcoded device metadata, or magic paths.
- Process, exec, fork, checkpoint, and restore preserve declared capability lifetimes and ownership.

## One execution lifecycle

Every backend and provider participates in the same lifecycle:

```text
discover capabilities
  -> construct MachineSpec
  -> validate and negotiate providers
  -> prepare resources transactionally
  -> supply launch-scoped authority
  -> spawn machine domain
  -> exec/control/observe processes
  -> update supported live policy
  -> stop and release resources deterministically
```

`MachineSpec` says what Linux environment is requested. It is safe to log, compare, persist, and validate.
Authority says which live host resources this launch may use and is neither serializable nor ambient.
Preparation resolves providers and acquires resources without exposing backend details. `Machine` is the live
control surface. Capabilities and validation connect all four stages so callers never discover a missing
feature after partially starting a guest.

The API must preserve this separation even when a backend could implement a shortcut. For example, a macOS
backend may lower a host directory directly and a Linux backend may use a mount namespace; both implement the
same typed projection. A provider may use a Unix socket, shared memory, or an in-process object internally;
the guest still observes the declared Linux handle/device/socket contract.

## How the API should evolve

When a use case cannot be represented:

1. Describe the Linux-visible behavior and lifecycle, not the current host implementation.
2. Find the smallest reusable atom missing from the existing spec, extension, authority, or control model.
3. Add a typed capability and validation rule before adding lowering.
4. Implement it per backend without branching on the caller or provider name.
5. Add backend-neutral contract tests and real guest conformance tests.
6. Advertise it only on backends that pass those tests.
7. Remove the caller workaround only after capability discovery proves the replacement exists.

Prefer extending an existing complete concept—such as `NamespaceEntry`, `HandleOperation`, `NetworkSpec`, or
`Machine`—over creating a parallel API. If an existing model advertises an operation that lowering rejects,
finish the implementation or stop advertising it; do not introduce a second narrower model for the working
subset.

## Existing Rust API

The current Rust engine already has the correct broad shape. Work from these contracts instead of replacing
them with a Husklet-specific facade:

- `Engine::{capabilities, validate, spawn, spawn_with_authority}` discovers and starts machines.
- `MachineSpec` contains guest CPU/process, identity, filesystem, namespaces, network, resources, security,
  time, entropy, translation cache, checkpoint, observability, debug, and extensions.
- `Machine` exposes initial process information, stdio/terminal ownership, wait, signals, pause, shutdown,
  attach, process enumeration, resource/network updates, hotplug, events, and force-stop.
- `ExtensionSpec` selects a provider/version/features and declares namespace entries, services, memory, and
  versioned configuration.
- `NamespaceEntry` models directories, files, symlinks, devices, host binds, sockets, and services.
- `Handles`/`OpenHandle` model open, read, write, positioned I/O, seek, truncate, metadata, ioctl, map, poll,
  and handle transfer with Linux credentials.
- `Memory`, `Events`, and `Lifecycle` model provider allocation/import/export, asynchronous events, and
  deterministic start/fork/exec/exit/stop hooks.
- `EngineCapabilities` and `Validation` model supported features, negotiated extensions, degradation,
  conflicts, limits, and host resource estimates.

The container repository already lowers `ContainerSpec` into this API. Its `Device` contract lets a product
compose mounts, process environment, engine extensions, and handle authority without teaching containers the
device domain. Husklet should select those devices; containers should validate and merge their requirements;
the engine should provide the mechanisms those requirements need.

## Container lowering boundary

Containers are not an alternate engine API. They add OCI/image/storage and container lifecycle semantics,
then lower those semantics into the engine's generic machine model.

```text
Husklet selects Device implementations
  -> Devices requests launch requirements
  -> containers merge image + process + mounts + resources + network + devices
  -> container runtime lowers one complete ProcessConfig
  -> engine validates and starts one Machine domain
```

The current device seam is deliberately domain-neutral:

```rust
pub trait Device {
    fn name(&self) -> &str;
    fn request(&self, context: DeviceContext<'_>) -> Result<DeviceRequest>;
}

pub struct DeviceRequest {
    pub mounts: Vec<Mount>,
    pub environment: BTreeMap<String, String>,
    pub extensions: Vec<ExtensionSpec>,
    pub authorities: Vec<HandleAuthority>,
}
```

This seam should evolve with engine authority types, but it must remain a composition seam rather than a
GPU/device taxonomy. A display, accelerator, hardware token, test filesystem, entropy source, or tracing
provider should all be expressible without adding variants to containers.

Containers must:

- keep selected `Device` implementations alive for every machine using their authority;
- request device requirements for the final guest/process context;
- merge requirements deterministically and reject path, provider, authority, service, and environment
  conflicts before engine spawn;
- pass engine extension configuration and authority without interpreting provider IDs or payloads;
- translate container mounts, identity, rootfs, overlay, resources, network, console, and process values into
  their corresponding typed engine fields;
- preserve engine validation errors with enough field/provider/path context for Husklet to present them;
- create one engine machine domain per running container and use domain exec for later processes;
- stop the machine domain before releasing device/provider authority or deleting storage.

Containers must not:

- recognize graphics, CUDA, Wayland, Docker, VPN, or Husklet setting names;
- turn extension configuration into environment variables or shell arguments;
- implement a missing socket, device, mapping, egress, or lifecycle mechanism itself;
- silently drop an engine field because Docker does not expose it;
- restart a separate machine merely because engine domain exec is unfinished;
- claim a device is active when engine validation degraded or rejected a required feature.

`DeviceRequest.mounts` and `environment` are compatibility channels for mechanisms that Linux processes
legitimately consume or that the engine cannot yet project. They are not the desired transport for device
operations. As engine sockets, devices, provider configuration, and loader metadata become complete, move
those requirements into extensions and remove the compatibility entries. Standard application variables
such as `DISPLAY`, `WAYLAND_DISPLAY`, or `DOCKER_HOST` may remain derived guest configuration; private
variables that select engine behavior must disappear.

The container API exposes the engine capability/validation surface. Before committing container state or
starting host services, Husklet validates the composed `ContainerSpec`, inspects selected/degraded
extensions, and reads effective limits. Containers call the engine's real validation path rather than
duplicating its rules.

The engine implements the generic mechanisms Husklet needs. Husklet's work is to compose and consume them
through containers rather than preserve compatibility mounts, private environment variables, or duplicate
lifecycle code.

## Capabilities Husklet consumes

| Capability | Husklet use |
| --- | --- |
| Namespace projections | Install driver libraries, manifests, generated files, sockets, services, and devices at exact Linux paths |
| Provider handles and memory | Back guest devices with typed I/O, ioctl, mappings, descriptor transfer, readiness, and shared resources |
| Provider lifecycle | Prepare host services transactionally and follow machine/process/fork/exec/exit ownership |
| PTY plus providers | Run an interactive terminal while graphics and other providers remain active |
| Writable projections | Expose explicitly authorized writable host files, directories, and endpoints |
| Network policy and transport | Apply workspace routes, DNS, VPN egress, forwarding, and live network changes |
| Machine domains and exec | Run terminals, applications, health checks, and tasks inside one container-owned execution domain |
| Resources and accounting | Enforce workspace limits and report effective use, pressure, OOM, and quota events |
| Live control | Enumerate and signal processes, update policy, attach streams, resize PTYs, and hotplug providers |
| Observability | Drive UI state and profiling from bounded typed events, metrics, traces, and faults |
| Time, entropy, cache, checkpoint, and debug | Configure advanced reproducibility, performance, recovery, and diagnostic workflows when selected |

### PTY means a controlling terminal

Byte transport and resize do not prove PTY compatibility. For every terminal process, the engine must create
a session, make the slave PTY that session's controlling terminal, place the process in a process group, and
make that group foreground before the program starts. The guest must observe all of the following:

- `isatty(0..2)` succeeds and `/dev/tty` opens;
- `/proc/self/stat` and `tcgetpgrp` identify the allocated terminal and foreground process group;
- the initial shell can enable job control without `EPERM`, `ENOTTY`, or `EBADF`;
- foreground/background transitions, `SIGTTIN`, `SIGTTOU`, `SIGTSTP`, `SIGCONT`, and hangup semantics match
  Linux;
- forked and executed descendants inherit the session and controlling terminal normally;
- every guest write to the terminal becomes readable by the host immediately, including a single byte
  without a newline; line buffering is application policy, not an engine transport policy;
- resize updates the terminal and delivers `SIGWINCH` to its foreground process group;
- closing the final host terminal handle produces EOF/hangup and deterministic process cleanup.

Husklet tests terminal behavior and process metadata separately through a second process in an existing
machine domain. On 2026-07-20 `interactive_shell_owns_a_controlling_terminal` passed: `tty` returned
`/dev/pts/0`, `isatty(0)` succeeded, Bash enabled job control, and `fg` completed a real foreground handoff.
The current macOS engine still fails `process_inventory_reports_terminal_and_foreground_group`: `ps` reports
`TTY=?`, `PGID=1`, and `TPGID=-1` for that same working shell. The remaining proven defect is therefore the
engine's guest process/proc metadata.

On 2026-07-24, `hl-engine 0.1.28` exposed a separate terminal-output defect. Husklet measured each input byte
crossing VTE, its worker, the Docker upgrade, the daemon, and `Terminal::write` in under one millisecond.
Kernel PTY echo from `cat` returned a single byte immediately. Bash Readline consumed the byte with the slave
in `-icanon -echo` mode, but its own one-byte redisplay write did not become readable from the terminal
master until a later newline or write. The engine must add a native regression test that launches an
interactive Bash, writes one ordinary character, and reads that character back without sending Enter.
Disabling Readline, locally echoing input, or replacing Bash with `sh` is not compatible behavior.

Full completion still requires unbuffered terminal writes plus signal, hangup, inheritance, and resize
conformance for `ProcessSpec { domain: Some(existing), terminal: Some(size) }`; no single shell probe proves
those contracts.

### Executable files remain executable across filesystem layers

`execve` must behave identically for an image file, a writable-layer file, and an authorized projected
file. Installing a package, compiling a program, downloading a tool, or copying an existing executable into
the workspace must produce a runnable file without rebuilding the image. This includes ELF interpreter and
auxiliary-vector correctness, executable mappings, dynamic linking, version resolution, shebang handling,
permissions, cache invalidation, and repeated exec after replacement.

The macOS conformance test copies image-native `/bin/true` into `/tmp`, marks it executable, and runs it. On
2026-07-20 that test passed through Husklet's container-backed terminal. This proves executable copying and
the image's existing dynamic-loader path, but not arbitrary newly installed or compiled programs. A separate
projected aarch64 C program previously crashed before `main` in glibc loader version resolution
(`ld-linux-aarch64.so.1+0x111a4`, null link-map metadata); keep that broader loader case independently tested.

### Package transactions must complete

Package managers exercise filesystem, process, signal, terminal, locking, rename, sync, and executable-loader
semantics together. A package transaction is conformant only when the package manager exits, its database is
consistent, the installed program executes, and teardown releases every process and writable layer.

On 2026-07-20 the live Ubuntu 24.04 Husklet test completed `apt-get update`, installed `hello`, executed the
installed program, and printed `PACKAGE_OK` in 9.54 seconds. The earlier apparent package-manager hang was a
guest `apt-get` process left alive after its host terminal worker was killed; it retained dpkg's lock. Husklet
now asks the generic exec attachment to kill its process on an unexpected disconnect, and a live regression
proves that an abruptly killed worker leaves no marked guest process behind. This fixes product cleanup but
does not weaken the engine requirement: domain process teardown, final-terminal hangup, and descendant
cleanup must remain deterministic for every caller. Keep both pipe-based and PTY-attached package tests so
filesystem/process defects remain distinguishable from terminal/session defects.

### Cold translation latency is observable behavior

Correct output after an unbounded warm-up is not sufficient for an interactive workspace. The engine must
report translation-cache hits, misses, compilation time, guest startup time, and first-use latency as typed
metrics, with separate budgets for cold and warm execution. Cache identity must include every input that can
change generated code, remain shareable across compatible workspace domains, and never trade correctness for
a stale hit.

On 2026-07-20 a cold `couchdb:3` scenario did not accept its first local HTTP connection within 60 seconds;
the immediately following scenario passed with the warmed translation cache. The conformance fixture allows
the cold run enough time to prove CouchDB behavior, but that allowance is not a performance pass. A release
gate must measure the cold and warm distributions independently and reject regressions against explicit
interactive and background-service budgets.

## Namespace and host endpoints

The extension vocabulary is the right abstraction. Husklet uses it instead of adding a graphics API.

```rust
ExtensionSpec {
    provider,
    namespace: vec![
        NamespaceEntry::HostBind(library),
        NamespaceEntry::Socket(wayland),
        NamespaceEntry::Device(render_node),
        NamespaceEntry::File(device_metadata),
    ],
    services,
    memory,
    ..
}
```

Semantics Husklet relies on:

- Install all entries atomically before the first guest instruction.
- Detect conflicts against rootfs, mounts, other providers, and standard Linux nodes during validation.
- Preserve byte-exact Linux paths; do not encode lists using delimiter-separated strings.
- Support regular files, directories, symlinks, Unix sockets, character/block devices, generated files,
  shared bytes, read-only binds, and writable binds as advertised features.
- Apply mode, uid, gid, file type, timestamps, xattrs, and link behavior consistently through
  `open`, `stat`, `readlink`, `readdir`, `/proc`, and `/sys` views.
- Make socket lifetime explicit: borrowed host listener, connected endpoint, or provider-created listener.
- Preserve Unix socket operations needed by Wayland and Docker: stream framing, half-close, poll/epoll,
  peer credentials, ancillary descriptor transfer, and nonblocking connect/accept.

Husklet must remove writable socket compatibility mounts. GL/Vulkan/CUDA libraries are read-only namespace
projections; Wayland, GPU transport, and Docker are typed socket/service entries.

## Provider registration and preparation

Providers are engine plugins, not application-side launch patches. Engine construction accepts a registry
of implementations; `MachineSpec` selects provider IDs, versions, features, and non-secret configuration.

```rust
let engine = Engine::builder()
    .provider(graphics)
    .provider(network)
    .build()?;

let launch = engine.prepare(&spec)?;
let machine = launch.spawn(io, authorities)?;
```

Preparation must:

- resolve every provider and negotiate an exact manifest version and feature set;
- ask each provider to validate its tagged configuration before any side effect;
- return the effective namespace, services, memory, authority requirements, and lifecycle;
- validate the combined result against the rootfs, standard namespace, resources, security policy, and
  every other provider;
- acquire resources transactionally and roll them back in reverse order on failure;
- bind the prepared launch to one spec digest so it cannot be reused under different policy;
- keep secrets and live host objects out of serializable specs and diagnostics.

Discovery is derived from registered providers and backend support. The engine must not hardcode known
provider IDs or claim a provider feature its runtime cannot lower.

## Generic provider authority

Provider authority contains only the ports selected for one launch:

```rust
pub struct ProviderAuthority {
    pub handles: Option<Arc<dyn Handles>>,
    pub memory: Option<Arc<dyn Memory>>,
    pub events: Option<Arc<dyn Events>>,
}

pub struct Authorities { /* ProviderId -> ProviderAuthority */ }

launch.spawn(io, authorities)
```

Providers return lifecycle ownership during preparation; callers grant only the host authority declared by
the prepared provider. Spawn rejects missing or excess grants. This keeps cleanup in the engine without
placing ambient authority in the spec.

Authority requirements:

- Validate provider, version, selected features, operations, quotas, and authority before side effects.
- Support every advertised `HandleOperation`: read, write, positioned I/O, seek, truncate, metadata,
  ioctl, map, poll, and transfer.
- Carry Linux credentials and deadlines on every request; cancellation must release blocked operations.
- Preserve open-file-description semantics across dup, fork, exec, close-on-exec, and descriptor passing.
- Map provider memory with declared protections, sharing, inheritance, coherency, and bounded lifetime.
- Deliver readiness/completion events without polling loops or unbounded queues.
- Run lifecycle callbacks in deterministic order and tear down all resources when the domain exits.
- Permit provider activation together with pipe I/O or a PTY; activation transport must be independent
  of stdio selection.

This supports a render node without engine GPU knowledge. A graphics provider can declare
`/dev/dri/renderD128`, answer typed ioctls, allocate/import memory, transfer descriptors, and publish
completion events. Device identity and sysfs/uevent files are provider namespace entries, never hardcoded
engine strings.

## Host services and distribution

The engine executes guest processes. It must not become a generic shell for starting product helpers on the
host. Classify every executable involved in a launch:

| Executable | Owner | Contract |
| --- | --- | --- |
| Linux workspace command | Engine machine/process domain | `MachineSpec.process` or `Machine::exec` |
| Engine runtime executable/backend | Engine distribution | Constructed and validated by the selected engine backend |
| Host implementation of an engine extension | Registered provider | `prepare`/authority/`Lifecycle`; never a guest-visible command path |
| Husklet daemon, updater, signer, or UI helper | Husklet application | Explicit product composition and platform adapter |
| Container image/build helper | Container/image domain | Typed container/image service, using engine guest execution when it runs in Linux |

A provider may internally use a host thread, process, socket, or in-process object. That is an implementation
detail behind its registered contract. The engine coordinates its lifecycle and authority but does not expose
`Command`, shell text, executable paths, or arbitrary host environment as extension configuration.

If a guest needs to connect to such a service, the provider declares a typed `ServiceRegistration` and
projects a `ServiceEntry`, `SocketEntry`, or `DeviceEntry`. The guest receives only the declared Linux object;
it must not learn how the host service was launched. Provider preparation fails atomically if its host
implementation is unavailable.

Engine distribution must also be explicit. A library may locate artifacts relative to its own verified
distribution, but it must not search Husklet bundles, `~/.hl`, `PATH`, or obsolete executable names. Prefer an
engine builder/configuration value that identifies the backend distribution and verifies architecture,
version, executable/library identity, permissions, and signing requirements before launch. Platform-specific
resolution stays in the backend adapter; the public engine contract remains portable.

Host service rules:

- no shell interpolation or command strings in engine/container specifications;
- no host executable selection through guest environment variables;
- no inherited host environment except an explicit backend-owned allowlist;
- no provider process outliving its prepared machine unless the declared lifecycle says it is shared;
- shared services use reference-counted leases and stop after the final machine releases them;
- startup readiness and failure are typed, deadline-bound, and included in transactional rollback;
- stdout/stderr are provider diagnostics, not control protocols;
- secrets enter through authority or a secret port and never command arguments, environment dumps, or specs.

For Husklet today, the GPU executor and compositor are host implementations selected by the application and
ultimately suitable for provider lifecycle. The Docker-compatible daemon remains product/container
composition unless it becomes a generic engine extension. Neither case justifies an engine API named after
those processes.

## Network and VPN

`NetworkSpec` expresses policy, while host mechanisms remain authority:

```rust
pub struct NetworkSpec {
    pub mode: NetworkMode,
    pub namespace: Option<Namespace>,
    pub interfaces: Vec<Interface>,
    pub routes: Vec<Route>,
    pub dns: DnsPolicy,
    pub egress: EgressPolicy,
    pub port_forwards: Vec<Rule>,
    pub external_listeners: bool,
}

pub enum EgressPolicy {
    Direct,
    Deny,
    Provider { provider: ProviderId, policy: Vec<RoutePolicy> },
}

pub trait NetworkTransport {
    fn connect(&self, request: ConnectRequest) -> Result<Box<dyn Socket>, NetworkError>;
    fn bind(&self, request: BindRequest) -> Result<Box<dyn Socket>, NetworkError>;
    fn resolve(&self, request: ResolveRequest) -> Result<Vec<Address>, NetworkError>;
}
```

The engine must not model `Socks5`, WireGuard, OpenVPN, or a VPN brand. Husklet selects a provider that
implements `NetworkTransport`; the spec selects routes and DNS policy. Provider configuration may name a
non-secret endpoint, while credentials, keys, and live tunnel handles remain launch authority.

Behavior Husklet relies on:

- IPv4 and IPv6 TCP, UDP, DNS, nonblocking connect, bind/listen/accept, socket options, and cancellation.
- Route selection by destination CIDR, protocol, and port, with explicit direct/deny/provider fallback.
- DNS policy that prevents leaks when egress is provider-routed.
- Stable virtual network identity shared by all processes in the container domain.
- Live replacement through `Machine::update_network`, with atomic cutover and structured failure events.
- Accurate `/proc/net`, interface ioctls, routing views, and source-address behavior.

Husklet maps workspace VPN settings to this policy and authority. Persisting the setting without applying and
reporting the effective engine network state is incorrect.

## Machine domains and exec

A container is one engine-owned domain, not a set of unrelated launches carrying the same string identity.

```rust
let mut machine = engine.spawn(spec, io, authorities)?;
let process = machine.exec(ProcessSpec { .. }, ProcessIo { .. })?;
```

The domain owns rootfs/overlay, namespace, network, provider grants, accounting, and cleanup. `exec` adds a
process with its own argv, environment, cwd, credentials, stdio/PTY, and limits. It must inherit the domain's
devices and mounts without re-registering providers or duplicating sockets. Health checks use the same API.

Husklet's live persistence contract writes a file in one terminal, closes it, and reopens the same workspace
and slot. On 2026-07-20 the file was missing. The prior implementation retained a new overlay for every
terminal but never reused any of them, leaking more than one hundred records without providing persistence;
automatic removal stops that leak but correctly makes the missing domain ownership visible. Completion
requires one durable workspace/container filesystem lease with terminal processes entered through domain
`exec`. Reintroducing per-terminal retained overlays or guessing the newest snapshot is not persistence.

Domain ownership must also survive or fail safely across a controller crash. A durable caller needs an
opaque, authenticated recovery token that can be persisted with its container record and later passed to
`Engine::{adopt,inspect,terminate}`. Recovery must prove that the token still identifies the same engine
domain; a host PID alone is unsafe because it can be reused. If adoption is unsupported, startup must
atomically terminate the verified old domain before marking its record exited.

On 2026-07-20 three prior Husklet domain-worker exits left three-process engine groups orphaned under host PID
1 for 36–59 minutes. Container state reconciliation correctly stopped claiming those processes were live,
but could not terminate them because the engine domain identity was not durable. The groups required guarded
host cleanup. A controller restart is not conformant until a live test kills the controller, restarts it,
then proves either adoption or complete descendant teardown without PID guessing.

Husklet now handles the non-crash case independently: on `SIGINT`, `SIGTERM`, or `SIGHUP` it stops accepting
domain API work, stops every active container, and only then exits. A live `SIGTERM` test removed the domain
worker and its three engine descendants within 20 milliseconds, and reopening the workspace preserved its
writable layer. This does not solve `SIGKILL`, abort, power loss, or controller crash recovery; those still
require the authenticated engine recovery contract above.

Expose typed process identities and support:

- enumerate processes;
- signal one process, a process group, or the domain;
- attach/detach streams repeatedly without stealing output;
- resize the selected PTY immediately;
- wait for one process or domain quiescence;
- graceful shutdown followed by deadline-bound force;
- fork/exec lifecycle notifications for provider resources.

Process enumeration must describe guest-visible processes rather than expose only reusable host PIDs. Each
bounded snapshot needs a stable engine process identity, guest PID and parent PID, process-group/session
identity, lifecycle state, credentials, command/argv, start and CPU time, resident/virtual memory, I/O
counters, controlling terminal, and an `initial` marker. Fields unavailable on a backend are typed as
unavailable; they are never replaced with zeroes or values copied from the launch specification. Snapshot
identity or sequence must let a caller detect races before signaling a row.

This is required by both Husklet's workspace process pane and the Docker-compatible `top`/stats API. The
current `Machine::processes()` result contains only `{ host_id, initial }`; that can prove membership but
cannot produce a guest process table. Containers must not fabricate PID 1, CPU/memory zero, start time, or
sleeping state while waiting for the richer generic contract.

## Filesystems and mounts

Core container storage remains `FilesystemSpec`; optional device artifacts remain extension namespace
entries. Both paths must share one VFS and conflict model.

Behavior Husklet relies on:

- image root, overlay lower/upper/work layers, read-only root, host binds, volumes, and ownership maps;
- read-only and read-write file/directory binds without delimiter/path restrictions;
- mount flags and propagation represented as enums/bitflags, not strings;
- coherent external writes and cache invalidation without exposing a generation-file path;
- Linux-compatible symlinks, hard links, rename, mmap, locking, xattrs, sparse files, and file watches;
- declarative `/proc`, `/sys`, `/dev`, `/tmp`, `/run`, resolver files, hostname, and device discovery;
- atomic launch rollback and deterministic cleanup for every owned layer and projected resource.

## Resources, security, and live control

Husklet and containers consume the advertised `ResourceSpec` fields: memory reservation/limit,
process/thread limit, CPU count/quota/affinity, open files, file size, locked memory, stack, address space,
and I/O rates. `ResourceUpdate` identifies mutable fields and returns effective values.

Accounting must report per-process and per-machine CPU, resident/virtual memory, I/O, process/thread count,
and limit violations. OOM, process-limit, provider-quota, and forced-shutdown events must be typed.

Security discovery must state sandbox compatibility with host networking, provider transport, executable
memory, descriptor passing, and projected nodes. Provider allowlists and budgets are enforced before
provider preparation.

## Observability

Husklet needs one bounded event stream, not stderr parsing or backend environment flags:

```rust
pub enum MachineEvent {
    ProcessStarted(ProcessInfo),
    ProcessExited(ProcessExit),
    ResourcePressure(ResourcePressure),
    NetworkChanged(NetworkState),
    Extension(ProviderEvent),
    Fault(Fault),
}
```

Events carry machine/domain/process identity, monotonic timestamp, sequence, and typed context. Queue limits,
overflow behavior, and sampling are capabilities. Metrics and tracing may be optional but must use the same
identities so Husklet can profile launch, syscall, GPU submission, frame presentation, and teardown latency.

## Capability discovery

Husklet consumes capability discovery rather than adding boolean guesses:

- namespace node kinds and bind access;
- socket operations and ancillary-data support;
- provider operations, memory types, PTY coexistence, lifecycle, and hotplug;
- network families, transports, route/DNS policy, and live mutation;
- domain exec/process control;
- resource launch/live/accounting fields;
- observability event kinds and queue limits.

`Validation` should return selected versions/features, optional degradation, namespace conflicts, estimated
resources, and effective policy. Errors identify category, field path, provider/path resource, and actionable
context. Backend-specific limits never leak into Husklet constants.

## Worked compositions

These are target-shape sketches, not permission to add domain names to engine core.

### Graphical Linux process

Husklet selects a compositor and GPU implementation. Their adapters implement generic extension providers.
The container device contributes:

- read-only projected driver libraries and a generated loader manifest;
- a projected Unix socket connected to the compositor provider;
- a provider-backed render character device;
- handle, memory, event, and lifecycle authority;
- ordinary guest loader/display variables derived from declared guest paths.

The engine validates all paths, operations, mappings, budgets, PTY coexistence, and provider authority before
starting the guest. It does not know whether the process is Chrome, GTK, Zed, or a test client; whether the
host GPU is Metal, WebGPU, Vulkan, or CPU; or whether the presentation host is macOS or Linux.

```text
Extension A: namespace files + loader manifest
Extension B: Unix socket endpoint + compositor service
Extension C: character device + handle/memory/event services
MachineSpec: process + rootfs + resources + extensions
Authority: live host implementations selected for A/B/C
```

The same three mechanisms can support a hardware security device with a management socket, a test filesystem
with projected libraries, or a remote accelerator. That reuse—not the word “provider”—is evidence that the
abstraction is generic.

### Docker-compatible client

Husklet/container composition owns the daemon decision. If a workspace may access it, the container device
requests one Unix socket projection at a guest path and derives the standard `DOCKER_HOST` value. The engine
implements Unix semantics and authority; it does not understand Docker requests or daemon lifecycle policy.

The same projection must pass an echo server, an arbitrary HTTP-over-Unix service, and Docker conformance.
If it works only because the engine recognizes `/run/docker.sock`, the API is wrong.

### VPN-routed workspace

Husklet owns the user setting and provider credentials. It selects a registered network transport and creates:

- typed route and DNS policy in `NetworkSpec`;
- non-secret, versioned provider configuration;
- launch-scoped connect/bind/resolve authority containing live tunnel state or credentials;
- a required feature selection so launch fails rather than silently using direct egress.

The engine applies policy to every process in the machine domain and exposes effective routes and typed
network events. It never parses a VPN URL, starts a named VPN product, or sets a private proxy environment
variable. The same transport contract can implement a test network, corporate gateway, recording proxy,
Tor-like route, or remote network stack.

### Additional terminal in a workspace

The container already owns a live `Machine`. Opening another terminal calls `Machine::exec` with a new
`ProcessSpec` and PTY. It does not rebuild the rootfs, prepare providers again, remount sockets, or copy an
opaque domain identity into a separate launch. Closing the terminal ends that process; stopping the workspace
ends the domain and all provider leases.

The same operation supports health checks, IDE tasks, container exec, background jobs, and test probes.

## Husklet migration ledger

The engine contracts exist; this table is Husklet's compatibility-removal ledger. A row is complete only when
Husklet uses the typed contract and the corresponding guest conformance test passes.

| Current Husklet mechanism | Required engine contract | Completion condition |
| --- | --- | --- |
| Mount Wayland socket read-write and set `WAYLAND_DISPLAY`/`XDG_RUNTIME_DIR` | `SocketEntry` plus typed guest endpoint placement | Wayland passes without a host socket bind; standard guest variables may be derived from the declared endpoint |
| Mount GPU transport socket and set `HL_GPU_EXEC` | Registered device provider with handles, memory, events, lifecycle, and `DeviceEntry` | GL/Vulkan/CUDA pass without private transport variables or application-installed device metadata |
| Bind GL/Vulkan/CUDA/NVML libraries and build loader variables | Read-only projections plus typed loader/device configuration | Provider namespace/configuration supplies libraries and manifests without delimiter-built environment strings |
| Mount Docker socket and set `DOCKER_HOST` | `SocketEntry` with Unix stream and credential semantics | Docker clients pass with no writable host bind mount |
| Launch each terminal/health command as another machine sharing a domain string | `Machine::exec` and typed process control | Processes share one namespace, network, device lifetime, accounting scope, and cleanup boundary |
| Store VPN settings without applying them | Routes, DNS, `EgressPolicy`, and launch-scoped `NetworkTransport` | TCP/UDP/DNS leak, cancellation, and reconnect tests pass; UI reports effective state |
| Spawn host daemon/native helpers from product code | Explicit host-service port or provider lifecycle when guest execution owns the service | Engine owns guest-facing lifecycle; unrelated product helpers remain Husklet composition |
| Select runtime behavior through private `HL_*` variables | Typed spec fields, provider configuration, or debug/observability APIs | Engine behavior has no private ambient configuration; unknown settings fail validation |
| Rely on hardcoded `/proc`, `/sys`, `/dev`, `/tmp`, or `/run` personalities | Declarative standard namespace profile with discovery | Validation reports every installed node and Linux behavior tests pass |
| Poll or parse stderr to infer runtime state | Bounded `MachineEvent` stream and accounting snapshots | UI and profiling use typed identities/events with documented overflow behavior |

## Review gate for a new engine API

Before accepting a new public type or operation, reviewers should be able to answer:

1. What Linux-visible behavior or lifecycle does it represent?
2. Which layer owns its policy, mechanism, authority, and cleanup?
3. Can a caller use it without importing a backend or product type?
4. Can at least two unrelated providers/backends implement it without engine name-based branching?
5. Is configuration typed or provider-versioned, bounded, and validated before side effects?
6. Are secrets and live host handles outside the serializable spec?
7. Does capability discovery express every meaningful combination and limit?
8. Are failure, cancellation, rollback, and teardown observable and deterministic?
9. Do contract tests cover the abstraction and real guest tests cover its Linux behavior?
10. Which Husklet workaround can be deleted only after those tests pass?

Reject an API when its main justification is “Husklet currently needs this exact object.” Restate the need as
a reusable Linux mechanism first. Also reject a nominally generic API when its only practical implementation
requires recognizing a provider name, parsing a private environment variable, or accepting an unchecked byte
channel.

## Conformance gates

An engine capability is complete only after a real guest test proves it and discovery advertises it:

| Use case | Required proof |
| --- | --- |
| Workspace terminal | PTY input/output, resize, signals, exec, fork, and teardown under provider activation |
| Chrome/GTK/Zed | Wayland socket projection, shared-memory and descriptor transfer, multiple windows/popups, clipboard and input under load |
| OpenGL/Vulkan | projected libraries/ICD, render-node ioctl/map/poll/transfer, frame presentation, resize, and process restart |
| CUDA/NVML | projected libraries, provider configuration, kernel submit/readback, memory limits, fork/exec, and truthful unsupported calls |
| Docker socket | projected host Unix socket, concurrent clients, peer credentials, descriptor lifecycle, daemon restart |
| Workspace mounts | read/write bind coherence, ownership, symlinks, mmap, locks, watches, overlay copy-up and whiteouts |
| VPN | routed TCP/UDP/DNS with leak tests, cancellation, reconnect, route replacement, and direct/deny fallback |
| Limits | each launch and live limit enforced, observable, and cleaned after restart |

Only after these gates pass should Husklet remove its corresponding mount, environment, subprocess, or
compatibility path and depend on the advertised typed capability.
