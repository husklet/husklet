# Host-services C ABI

This is the portability contract. It should be smaller and more semantic than either POSIX or the Linux syscall
table. A one-for-one `host_syscall(nr,args)` escape hatch is forbidden: it would leak Linux-host behavior into the
portable ABI and make Windows impossible.

## ABI envelope

```c
/* include/hl/host_services.h */
#ifndef HL_HOST_SERVICES_H
#define HL_HOST_SERVICES_H
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define HL_HOST_SERVICES_ABI 1u
typedef uint64_t hl_host_handle; /* opaque, 0 is invalid */

typedef struct {
    int32_t code;       /* HL_HOST_* domain, never errno/GetLastError */
    uint32_t reserved;
    uint64_t value;     /* byte count, handle or operation-specific value */
} hl_host_result;

typedef struct hl_host_services {
    uint32_t abi;
    uint32_t struct_size;
    uint64_t capabilities;
    void *context;
    /* append-only function-pointer groups follow */
} hl_host_services;

int hl_host_services_validate(const hl_host_services *services, uint64_t required_caps);

#ifdef __cplusplus
}
#endif
#endif
```

Every public struct starts with ABI/version and byte size; extensions append fields. Reserved fields must be zero.
The engine copies/validates the table at creation and never assumes a newer tail exists. Function pointers receive
`context` explicitly. No host implementation uses engine globals.

`hl_host_result.code` has a small stable domain such as OK, NOT_FOUND, EXISTS, ACCESS, INVALID, WOULD_BLOCK,
INTERRUPTED, NO_SPACE, NOT_SUPPORTED, IO, RESOURCE_LIMIT and PLATFORM_FAILURE. Linux ABI performs the final mapping
to Linux errno based on the operation. Raw `errno`, Mach codes, NTSTATUS and `GetLastError` remain available only in
diagnostic detail, never as guest-visible results.

## Handle and memory ownership

- Host handles are opaque tokens scoped to one `hl_engine` and one host-service context. They are not guest fd/pid
  numbers and need not be native descriptors.
- `linux-abi` owns guest fd allocation and OFD semantics: shared offsets/status flags, dup/fork inheritance,
  close-on-exec, epoll identity and final-close behavior.
- Buffers are caller-owned for the duration of a synchronous call. Asynchronous operations use an engine-owned
  request object or copy; the host never retains arbitrary guest pointers.
- Each creation returning a handle has one idempotence policy and one explicit close/release call. Destruction of
  the engine drains/cancels outstanding operations before destroying the host context.
- Host backends provide fault-injection hooks in tests, not production-global environment switches.

## Service groups

The actual header should be split into append-only sub-tables so a backend can advertise groups and sizes.

| Group | Semantic operations | Linux ABI remains responsible for |
|---|---|---|
| memory/JIT | reserve, map file/anonymous, protect, unmap, dual-map executable cache, publish icache, query page size | guest VMAs, Linux flags/errors, brk, mremap, W^X policy visible to guest |
| file/path | open-relative, read/write-at, truncate, sync, metadata, directory iteration, link/rename/unlink, advisory lock | rootfs/overlay resolution, Linux flags/stat layout, OFD offset and mount/proc model |
| process | spawn isolated runner/process, wait, terminate, suspend/resume, duplicate inherited host resources | guest pids/groups/sessions, clone/fork/exec semantics, wait status/rusage |
| thread | create/join, TLS key, park/wake, interrupt blocking operation | guest tids, clone flags, robust futex, clear-tid and signal masks |
| event | create pollset, add/modify/delete interest, wait, monotonic wake token | epoll edge/level/oneshot/exclusive rules, fd identity, timer/signalfd/eventfd/inotify state |
| clock/random | monotonic/realtime/CPU clocks, sleep deadline, secure bytes | Linux clock ids, timer slack, time64 structs and restart behavior |
| network | socket/connect/bind/listen/accept/send/recv, options in host-neutral enums | Linux domains/options/errors, netns, netlink synthesis, port publishing policy |
| host identity | stable file identity, process resource sample, CPU/memory topology | synthetic `/proc`/`/sys`, cgroup limits, guest uname/cpu feature claims |
| shared IPC | named/shared memory object, cross-process wait/wake primitive | SysV/POSIX namespaces, Linux permissions/limits and futex keys |
| optional GPU | allocate/export/import render resource, generation/identity | Linux render-node ioctl contract and guest-visible dmabuf identity |
| diagnostics | structured log/event sink, crash context capture | stable event ids and redaction; never correctness behavior |

Capabilities declare optional groups (`GPU`, checkpoint acceleration, native process cloning), not ordinary functions
that happen to be hard on one host. Missing optional capability must yield a defined Linux-visible unsupported result
or select a portable engine implementation.

## Process and fork rule

The universal interface must not require `fork`. Define “create a child engine process from serialized engine state
and inherited host handles” as the semantic operation. macOS/Linux may optimize it using fork after quiescing; Windows
can spawn a runner and reconstruct state. Guest `fork()` semantics belong to `linux-abi` and the engine snapshot model.

Initially, retain the existing macOS fork implementation behind `host-macos` and mark `HL_HOST_CAP_FAST_CLONE`.
Before host-windows, all correctness must pass with the capability disabled using serialize/spawn/restore. This also
removes Objective-C/framework fork-safety assumptions from engine core.

## Event rule

Do not expose kqueue, epoll, poll or IOCP structures in the interface. The host pollset reports stable readiness
tokens and host-neutral readiness bits. `linux-abi` owns:

- current interest and OFD identity;
- level re-priming versus edge transitions;
- one-shot disarming and exclusive wake policy;
- close/dup/fork behavior;
- Linux event bit translation and ordered copyout.

The Chrome lost-wakeup history is a mandatory cross-backend conformance scenario for this boundary.

## Reentrancy, cancellation and locking

1. Calls state whether they may block. Blocking calls accept an engine cancellation token/deadline.
2. Host callbacks enqueue completion; they never enter guest execution directly.
3. The engine calls no host function while holding translator cache locks or guest fd/process registries unless that
   function is explicitly nonblocking and documented lock-safe.
4. After fork/clone, the backend receives a child-rebind lifecycle event for pollsets, watchers, cached host handles
   and framework state.
5. Destroy is bounded: cancel, drain, close handles, destroy context. Leaks and detached host threads fail tests.

## Contract tests required of every backend

One host-neutral C suite is linked against macOS, Linux, Windows and a deterministic fake backend. It covers table
size/version skew, missing capabilities, handle exhaustion/reuse, partial I/O, interruption, deadlines, cancellation,
cross-thread wake, close-during-wait, child rebind, file identity, error translation inputs and allocation failure.
Passing native host unit tests without this common suite does not qualify a backend.
