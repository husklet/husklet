# Engine and Rust surface API

## Public C lifecycle

The engine API is intentionally smaller than the host-services table. Configuration is data, execution is an opaque
instance, and control is asynchronous-safe without exposing internal CPU or Linux ABI structs.

```c
/* include/hl/engine.h */
#define HL_ENGINE_ABI 1u

typedef struct hl_engine hl_engine;

typedef enum {
    HL_GUEST_AARCH64 = 1,
    HL_GUEST_X86_64 = 2
} hl_guest_isa;

typedef struct {
    uint32_t abi;
    uint32_t struct_size;
    hl_guest_isa guest_isa;
    uint32_t flags;
    uint64_t memory_limit;
    uint32_t pid_limit;
    uint32_t cpu_limit;
    const void *payload;
    size_t payload_size;
} hl_engine_config;

typedef struct {
    uint32_t abi;
    uint32_t struct_size;
    int32_t kind;
    int32_t guest_status;
    uint64_t detail;
} hl_engine_exit;

int hl_engine_create(const hl_engine_config *, const hl_host_services *, hl_engine **out);
int hl_engine_run(hl_engine *, int argc, const char *const argv[], hl_engine_exit *out);
int hl_engine_request(hl_engine *, uint32_t request, const void *data, size_t size);
void hl_engine_destroy(hl_engine *);
const char *hl_engine_version(void);
uint32_t hl_engine_abi(void);
```

The final config may use the existing validated offset/string-pool wire as `payload` during migration. Do not expose
Rust `repr(C)` structs containing pointers, platform `pid_t`, native fds or enums with compiler-dependent size.

The replacement payload must encode the complete typed machine specification: root/overlay, mounts and volumes,
process/identity/PTY, namespaces, networking, resource/security policy, cache/checkpoint/observability policy, and a
versioned list of extension specifications. Dedicated product flags such as `gpu_iosurface`, `render_node`, VPN kind,
or compositor backend are forbidden. A domain provider instead contributes namespace entries, provider-backed open
services, memory/resource requirements, guest files/libraries/environment and lifecycle policy through a generic
extension specification. See [`../engine-extension-capabilities.md`](../engine-extension-capabilities.md) for the
high/low-level API and Linux facility checklist.

Errors from API calls are engine-domain values. Guest exit/signal/core status is returned in `hl_engine_exit`, not
collapsed into a host process exit code. Diagnostic strings are copied into caller buffers or emitted through the
structured sink; no borrowed global error string crosses threads.

## Runner boundary

Phase 5 produces `hl-engine-runner`, a tiny C executable linked with one host backend and the selected host-CPU
translator library. It validates a versioned config/control channel, creates one engine and exits with a documented
runner status. Guest status travels on the control channel.

This remains the default integration even though `libhl-engine.a` exists. The current runtime installs process-wide
signal/Mach handlers, uses fork, owns global tables and may call `_exit`. Keeping those effects in a child process
protects the Rust daemon and permits independent crash/restart, codesigning and resource accounting.

In-process use is supported only after all of these are true:

- two engine instances run concurrently without shared mutable state;
- create/destroy restores signal handlers and closes every host handle/thread;
- no engine path calls `exit`, `_exit`, changes process cwd/environment, or assumes it owns fd 0/1/2;
- fork-dependent paths are absent or coordinated through an embedder callback;
- sanitizer and repeated create/run/destroy stress is clean.

## `hl-engine` Rust crate

`hl-engine` replaces the low-level role of `dd-jit-darwin`; higher container modeling can remain in a separately
named Rust runtime crate.

```text
pub enum GuestIsa { Aarch64, X86_64 }
pub struct EngineArtifacts { runner: PathBuf, abi: u32, host: HostKind }
pub struct LaunchConfig { /* owned MachineSpec: mounts, process, network, limits, extensions */ }
pub struct SpawnIo { /* OwnedFd/borrowed-fd distinctions */ }
pub struct Child { /* pid/process handle + control channel */ }
pub enum Error { NoArtifact, AbiMismatch, InvalidConfig, Spawn, Engine, Protocol }
```

Rules:

- Raw declarations stay private in `ffi.rs`; safe wrappers own all buffers for the duration required by C.
- Use `OwnedFd`/`BorrowedFd` on Unix and corresponding owned handles on Windows. A raw integer is never implicitly
  transferred. Each method documents duplicate/borrow/consume behavior.
- Artifact selection is `(host OS, host CPU, guest ISA, engine ABI)`, not a `Guest` enum containing a guest OS. Linux
  is the sole portable personality.
- Runtime discovery checks an explicit directory, packaged resource metadata and development build output. A baked
  path is diagnostic fallback, not a release contract.
- Launch config encoding has one implementation and golden byte fixtures shared with C. Environment-variable flag
  soup is not a second control plane.
- Checkpoint, pause, signal, wait and termination use the runner control API. Rust must not hardcode macOS signal 29
  or mutate trigger files as its portable contract.
- `hl-engine` contains no Linux syscall semantics, translator fallbacks, pcache format logic or host-service policy.

## Compatibility transition

1. Add `hl-engine` beside existing crates and implement adapters preserving the current Rust names where necessary.
2. Move config wire tests to a shared C fixture plus Rust encode/decode parity.
3. Make `dd-jit` depend on `hl-engine`, not `dd-jit-darwin`; keep temporary deprecated re-exports.
4. Move macOS artifact build/sign/package code out of Cargo into `engine/` build/install targets consumed by
   `hl-engine/build.rs`.
5. Remove `dd-jit-darwin` only after every direct consumer is migrated and native-Darwin guest disposition is
   explicit.

The rebrand phase owns final public names and compatibility aliases. Phase 5 uses `hl_*`/`hl-engine` as the target
surface but must coordinate the atomic product-name cutover rather than shipping mixed persisted paths or env names.
