# Current-state validation

Baseline: main at `68009c37`, inspected 2026-07-13.

## What exists

`dd-jit-darwin/src/runtime` is already C, but it is built as three macOS unity executables rather than
independent libraries. `build.rs` invokes macOS Clang, links IOSurface/CoreFoundation for every Linux target,
codesigns the result with JIT entitlement, and bakes executable paths into Rust environment variables.

Approximate maintained C/header size by current directory:

| Area | Lines | Present role |
|---|---:|---|
| `engine` | 2,207 | cache, dispatch and trampolines |
| `translate` | 16,738 | aarch64/x86 frontends, ARM64 emission, pcache, signal frames |
| `host` | 125 | ARM64 instruction emitter, not a host-OS backend |
| `os/linux` | 37,478 | Linux personality mixed with macOS implementations |
| `os/darwin` | 1,467 | native Darwin guest, spawn shim and jail |
| `targets` | 1,593 | unity composition, initialization, faults and CLI entrypoints |
| `include` | 474 | CPU structures, GPU and launch ABI |

The two Linux target files textually include 20–25 `.c` files in a required order. Shared private globals,
forward declarations and macros cross those file boundaries. This makes files look modular while the compiler
still sees one translation unit; compiling any layer independently is not currently a supported operation.

## Current external surface

- Rust `dd-jit` publicly re-exports types from `dd-jit-darwin`; despite its description it has an unconditional
  macOS-backend dependency.
- `dd-jit-darwin` exports `Guest`, `LaunchConfig`, `SpawnConfig`, `spawn`, `spawn_io`, volumes and port maps.
- `Guest` currently includes Linux/aarch64, Linux/x86_64 and Darwin/aarch64.
- The C header exposes a 128-byte, size/version-prefixed `ddjit_config` wire and `ddjit_spawn`. This is a useful
  compatibility seed, but it mixes engine configuration with a Darwin `posix_spawn` implementation.
- Each engine executable exposes `main` and `dd_run`; `dd_run` is single-shot and relies on process globals.
- Cargo/build scripts own compilation, signing and artifact discovery. There is no installed C library,
  pkg-config/CMake metadata, symbol-version policy or standalone C test target.

## Coupling that blocks portability

The Linux ABI area directly uses host facilities throughout its implementation. Concentrated examples are
`container/vfs.c`, `syscall/event.c`, `fs.c`, `io.c`, `proc.c`, `thread.c` and `checkpoint.c`.

| Linux concept | Current macOS mechanism | Why direct replacement is unsafe |
|---|---|---|
| `epoll`, timerfd, inotify, signalfd/eventfd | kqueue, pipes and side tables | Linux readiness, OFD sharing, fork and close semantics differ from both kqueue and Windows waits |
| VM and JIT mappings | `mmap`, Mach VM, `MAP_JIT`, cache-control APIs | W^X, dual mapping, fault delivery and instruction-cache publication are host policies |
| processes/threads/signals | `fork`, pthreads, Mach/POSIX signal hooks | Windows lacks fork; host signal numbers and interruption behavior are not Linux ABI values |
| filesystem/metadata | Darwin fds/stat/fcntl plus overlay and synthetic tables | flags, inode identity, locks, rename behavior and stat layout differ |
| `/proc` and pid relationships | synthetic data plus macOS process inspection/registries | peer visibility must use the engine process model, not host `/proc` availability |
| networking | BSD sockets plus virtual loopback/bridge state | error/options/readiness translation and isolation must remain Linux-defined |
| GPU render node | IOSurface/CoreFoundation | optional platform service, not part of Linux syscall core |
| checkpoint/forkserver | host fork, shared mappings and host signals | cannot be a required primitive for a Windows backend |

Platform-token inspection finds direct macOS/host-service usage in two current `engine` files, eight translator
files and at least 22 `os/linux` files. Removing includes alone would only expose unresolved dependencies.

## Architectural assets worth preserving

- One Linux syscall family dispatcher shared by both guest ISAs.
- Guest CPU structures and `G_*` architecture seams already isolate some register/syscall-number differences.
- A skew-safe config prefix (`magic`, `pool_len`, `header_len`, `abi`) and behavioral Rust byte-layout tests.
- Extensive Linux guest C fixtures and Rust orchestration covering syscall, ELF, pcache, overlay, forkserver,
  container lifecycle and regressions.
- Mature fixes around OFD sharing, pid namespace membership, seccomp, signal frames, SMC, cache reclamation,
  event readiness, high fds and cross-process futexes. These are compatibility requirements, not cleanup noise.

## Current hazards for a split

1. Static/process globals assume one engine instance and frequently encode ownership implicitly.
2. Unity include order substitutes for headers and creates cross-file private-symbol reachability.
3. The x86 frontend and aarch64 frontend share Linux code but have different pcache, ELF and translation details.
4. The internal representation is not a stable, architecture-neutral IR today; much translation directly emits
   ARM64. Inventing a large IR in one step would change hot-path behavior and cache formats.
5. Host `errno`, fd, pid, signal, clock and stat values sometimes sit beside guest values. The boundary must name
   which domain every value occupies.
6. The runner uses fork, process-wide handlers, TLS and `_exit`; linking it into a multithreaded Rust daemon today
   would be less safe than supervising a dedicated process.
7. Existing audit branches contain useful research but are based on older trees. They are evidence only and must
   not be cherry-picked wholesale.
