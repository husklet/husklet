# Target architecture

## Repository shape during extraction

```text
engine/
  CMakeLists.txt
  meson.build                 optional only after CMake/Make path is proven
  include/hl/engine.h         public lifecycle ABI
  include/hl/config.h         versioned config/wire ABI
  include/hl/host_services.h  host-service table
  src/core/                   engine state, dispatch, cache, lifecycle
  src/translator/
    ir/                       private versioned IR and validation
    guest/aarch64/            Linux guest decode/lower
    guest/x86_64/
    host/aarch64/             IR selection/emission
    host/x86_64/
  src/linux_abi/
    syscall/                  canonical Linux-number dispatch and argument decode
    process/                  guest pid/thread/signal/credentials model
    fd/                       guest fd table and open-file descriptions
    fs/                       overlay, path resolution, metadata and proc/sys synthesis
    memory/                   guest VMA and Linux protection semantics
    event/                    epoll/eventfd/timerfd/signalfd/inotify models
    net/                      Linux socket/options/netns model
  src/host/common/            ABI validation and host-neutral helpers only
  src/host/macos/
  src/host/linux/
  src/host/windows/
  src/runner/                 small executable linked to the selected host backend
  tests/c/                    ABI, compatibility, lifecycle and fault-injection tests
  tests/fixtures/             small Linux ELF/C sources; no opaque host workspace
  perf/                       maintained performance workloads and baselines
  tools/                      IR/cache inspection and test utilities
  LICENSE
  README.md

hl-engine/
  Cargo.toml
  build.rs                    locates/builds C artifacts; does not own engine sources
  src/ffi.rs                  private raw declarations
  src/lib.rs                  safe public Rust API
  tests/surface.rs            ABI/version/ownership/error smoke coverage only
```

`engine/` must be copyable into its own repository without importing Rust source. Until the split, its build can
be invoked by Cargo, but CMake/Make remains authoritative and emits libraries, runner, headers and metadata.

## Library products

| Product | Contents | Allowed dependencies |
|---|---|---|
| `libhl-translator-<hostcpu>.a` | guest decoders, private IR, selected host-CPU lowering/emission, cache format | C runtime and core headers; no host OS headers/calls |
| `libhl-linux-abi.a` | Linux syscall semantics, process/OFD/VMA/proc/sys models | core + `host_services.h`; no platform headers |
| `libhl-host-macos.a` | Mach/kqueue/BSD/IOSurface implementations | macOS SDK/frameworks |
| `libhl-host-linux.a` | Linux host implementation | libc/Linux host APIs; must still translate guest state rather than blindly pass through |
| `libhl-host-windows.a` | Win32/NT implementation | Windows SDK |
| `libhl-engine.a` | lifecycle and composition of translator + Linux ABI + selected host table | the above static libraries |
| `hl-engine-runner` | config/control protocol and one isolated engine instance | `libhl-engine` plus selected host backend |

A shared library may be added after ABI discipline is proven. Static libraries first reduce loader/signing complexity
and make platform linkage explicit. Public symbols use an `hl_` prefix and hidden visibility by default.

## Dependency rules

```text
runner -> engine core -> translator
                    `-> linux-abi -> host-services interface <- host backend
```

- Translator may report exits/faults and request executable-memory/cache operations through narrow core callbacks;
  it must not call the Linux syscall layer or host directly.
- Linux ABI owns guest-visible identity and semantics. It requests semantic host primitives, then translates host
  results to Linux results.
- Host implementations never decode guest pointers, syscall numbers or Linux structs. Buffers passed to a host call
  are engine-owned host memory with validated lengths.
- Host services never call back into an arbitrary guest context while holding host locks. Completion enters through
  an engine-controlled poll/wake boundary.
- No platform `#ifdef` above `src/host/<platform>` or the small build-selection file. Guest-ISA selection is separate
  from host-OS selection.

## Translator qualification

The requested frontend -> IR -> host backend shape is the target, but current translators directly produce ARM64.
Migration therefore uses a private, versioned IR incrementally:

1. Define control exits, memory accesses, calls, faults, safepoints and architectural state transitions first.
2. Move instruction families into IR only with native/QEMU differential and emitted-code performance proof.
3. Keep a temporary direct-ARM lowering adapter behind the same translator interface; do not claim it as portable.
4. Persistent caches include guest ISA, host ISA, IR ABI, codegen ABI, feature bits and all code-changing modes.
5. Delete the direct adapter only after both guest frontends pass through the IR and at least two host-CPU backends
   prove the abstraction. Do not freeze the private IR as public API.

## Platform matrix

| Host OS | Host CPU | Linux/aarch64 guest | Linux/x86_64 guest | Phase 5 expectation |
|---|---|---|---|---|
| macOS | arm64 | existing oracle | existing oracle | first extracted backend; zero behavior/perf regression |
| Linux | arm64 | new | new | first portability proof; reuse ARM64 backend with Linux host services |
| Linux | x86_64 | new backend required | new backend required or safe same-ISA path | follows host-linux service completion |
| Windows | arm64 | new | new | host-services contract validation; no fork assumption |
| Windows | x86_64 | new backend required | new backend required | last backend, after process/event abstraction is proven |

Host Linux is not allowed to bypass `linux-abi` by forwarding arbitrary syscalls. That would produce a different
security, fd, pid, procfs and overlay model and defeat cross-host compatibility.

## Native Darwin guest disposition

`jitdarwin` and `darwinjail` do not enter `libhl-linux-abi`. During migration they remain in a compatibility package
with their existing Rust selector and tests. A later explicit product decision may move them to `legacy/`, define a
separate Darwin ABI personality, or retire them. Linux portability work must not pretend this existing public variant
never existed.
