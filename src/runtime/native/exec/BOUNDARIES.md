# Native translated execution boundary

> Historical replacement design. The `exec/` kernel is not selected by the
> product. Production uses the retained C closure in the sibling `retained/`
> tree through Rust's `hl-engine/src/execution/` worker boundary. References
> below to `hl-execution`, `ExecutionMachine`, `native/executor.rs`, or a Rust
> production guest executor describe the deleted implementation and are kept
> only as migration rationale.

This directory preserves a candidate translated-block kernel designed while
Rust owned guest lifecycle, scheduling, and policy. It is not a Cargo runtime
package and must not be described as the current product engine.

## Why this boundary exists

The deleted Rust production path called `ExecutionMachine::run_slice`. An
x86-64 instruction is fetched, decoded, and interpreted on every step.
AArch64 retains a 256-slot cache of at most 64 decoded instructions, but only
register-only instructions execute from it; any memory operation returns to
single-instruction decode and interpretation. This is correct but it is not the
retained engine's translated execution path and accounts for the DBT, soak, and
core-workload timeouts.

The retained engine performs one coarse cycle:

```text
guest PC -> block-map lookup -> translate on miss -> publish executable bytes
         -> resolve cache generation -> run_block -> block_return
         -> fully spilled typed exit -> syscall/signal/scheduler
```

Generated loads and stores address the projected guest mapping directly. They
must not call a Rust trait per instruction. A callback at that frequency would
replace interpreter decode overhead with an FFI overhead and would make faults,
signals, fork, and mapping replacement impossible to reason about.

## Retained dependency map

The read-only oracle is `../engine`.

| Retained source | Responsibility | Migration disposition |
| --- | --- | --- |
| `src/translator/emit.h`, `arena.{h,c}`, host AArch64 assembler | Bounded emission and RW/RX address conversion | Mechanically transferable after service types are narrowed |
| `src/translator/guest/*/cpu.h` baked prefix | Register, exit, host-save, and vector offsets consumed by emitted code and assembly | Transfer through the generated CPU schema; never hand-copy offsets |
| `src/core/dispatch.c` `run_block`/`block_return` | Callee-save preservation and native entry/return ABI | Mechanically transferable only with schema assertions |
| `src/translator/guest/aarch64/stubs.c` | Prologue, spill, typed exits, direct/indirect chain stubs | Mechanically transferable with the AArch64 frontend |
| `src/translator/guest/aarch64/translate.c` | AArch64 block formation and host-code emission | Transferable translator semantics; old globals and Linux mapping hooks require extraction |
| `src/translator/guest/x86_64/{emit,address,translate}.c` and `lower/` | x86-64 decode/lowering to AArch64 | Transferable translator semantics; helper exits require typed ownership |
| `src/translator/guest/x86_64/translit/` | Same-host x86-64 copied-code blocks | Transfer later for x86 hosts; not used to pretend cross-ISA coverage exists |
| `src/translator/cache.c` map, provenance, publication, generations | Reusable block identity, W^X publication, lookup, rollover | Extract into instance state; the process globals are not transferable |
| architecture `cache.c` | Persistent cache format, relocations, ABI validation | Native C/assembly ownership is permanent; any future import must use the live executor boundary and its ABI tests, not a parallel safe-Rust parser |
| `src/core/dispatch.c` `run_guest` | Cache miss, publication, execution, safepoints, and exit selection | Extract the block cycle; do not import Linux/personality policy |
| `src/core/target/{aarch64,x86_64}.c` | Unity composition of translator, Linux ABI, loader, process, and host policy | Oracle only; never copy as the native boundary |

The following retained state must become fields of one executor, not mutable
process globals: emission arena and cursor, code-mapping handle and alias delta,
block map and generation, pending chain patches, indirect-branch cache,
instruction provenance, translated source ranges, fatal status, publication
services, and cache diagnostics. Per-thread execution generation and
`in_translated` belong to an executor-owned thread registry.

## CPU and exit ABI

`src/schema/cpu` is the only source of baked native offsets. Generated C and
Rust views must agree before native sources can link. Stage one covers the
prefix touched by entry, prologue, spill, and ordinary block exits:

| Field | AArch64 offset | x86-64 offset |
| --- | ---: | ---: |
| guest general registers | 0 | 0 |
| guest PC | 256 | 128 |
| flags | 1024 | 136 |
| TLS bases | 264 | 144, 152 |
| exit reason | 272 | 160 |
| saved host SP | 280 | 168 |
| saved host GPRs | 288 | 176 |
| saved host vectors | 896 | 272 |
| guest vectors | 384 | 400 |
| x86 scratch | n/a | 656 |

The complete Rust architectural snapshots are not FFI layouts. A native CPU is
an explicitly `repr(C)` execution record populated before entry and copied back
after exit. Extending its tail is permitted only by extending the schema and
both assertion suites. Reinterpreting `Aarch64CpuState` or `X86CpuState` as the
C record is forbidden.

The public native exit is architecture-neutral: continue/branch, syscall,
interpreter fallback, synchronous fault, asynchronous interrupt, cache epoch
change, yield, and fatal engine error. Architecture-specific retained reasons
such as x86 AVX, x87, REP, or AArch64 software-TLB miss remain private and must
be resolved inside the native kernel or converted to a precise fallback at the
faulting PC. The scheduler never switches on retained numeric reason values.

## Cache and block lifecycle

1. Lookup is by guest PC plus executable mapping epoch. The selected entry
   retains its cache generation until native return.
2. A miss translates while holding the executor's publication lock. Emission
   is bounded before decode begins.
3. Publication validates that the byte range belongs to the current mapping,
   flushes the executable alias, then publishes map/chaining metadata. Readers
   cannot observe metadata for unpublished bytes.
4. A block enters only through the generated prefix ABI and exits only through
   its spill stub. A native fault uses the provenance map to recover the exact
   guest instruction before control reaches Rust.
5. Executable writes invalidate every block whose decoded source overlaps the
   changed range. A mapping epoch change invalidates stale lookup identities.
6. Cache rollover allocates a fresh generation. A retired generation remains
   mapped while any registered thread may execute or reach it through a chain.
7. Fork closes admission, reaches dispatcher safepoints, repairs or discards
   inherited aliases and locks in the child, and exposes no half-published
   generation. Exec and restore discard identities from the former image.
8. Destroy stops admission, drains native entries, unmaps current and retired
   generations, and releases the instance exactly once.

Allocation ownership follows the same lifecycle. The executor owns its cache,
indirect-branch table, and arena mapping; the cache owns each lookup,
provenance, liveness, relocation, and resolved-relocation array. A registered
direct token owns one allocation until unregister; an interrupt token owns one
until its explicit destroy; and each attached fault thread owns one alternate
stack until detach. Executor destroy refuses a live direct token. Constructor
failure unwinds these owners in reverse order. Fork repairs inherited mappings
and retires direct authority but does not create a second allocation owner;
parent and child subsequently release only their private process copies.
IBTC storage is an executor-owned aligned allocation rather than part of the
public execution ABI or generated CPU layout. `src/ibtc/storage.c` pairs C11
`aligned_alloc` with `free` and, for the MSVC runtime, `_aligned_malloc` with
`_aligned_free`; callers cannot mix those ownership families. Its fixed size is
an exact multiple of its 64-KiB alignment, construction reports allocation
failure through `HL_NATIVE_MEMORY`, and executor teardown releases it once.
The retained ownership comparison studied
`src/translator/cache.c::jit_cache_init`, `cache_rotate`, and
`jit_after_fork`, the exec/fork callers in `src/linux_abi/thread.c` and
`src/linux_abi/fork.c`, and the alternate-stack/fault publication owners in
`src/core/dispatch.c` and `src/linux_abi/syscall/proc.c`. Unlike that
process-global oracle, Rust owns one executor-local arena, cache, IBTC, and
generation lifecycle; the host callback owns both aliases and its backing
descriptor. The allocation lifecycle gate injects each heap failure, malformed
mapping metadata, and repair failure. It also runs 16 post-warmup
create/fork-repair/destroy cycles and requires unchanged Linux mapping,
descriptor, and task counts with resident growth bounded to 64 MiB under ASan.

Memory access mode and a descriptor-qualified authority identity are part of
every translation identity. The identity is reused only when guest range, host
delta, permissions, and mapping/instruction generations are byte-for-byte
equivalent. An executor admits only one such identity at a time. Changing either
value takes exclusive mutation admission, rotates the cache generation, clears
the indirect-branch table and all pending/resolved relocations, and rejects
cross-identity backpatches. Fork repair discards a non-guarded identity before
the child can run. This deliberately mirrors the retained engine's whole-cache
rotation on soft/direct-mode transitions while removing its process-global
mode state. The fields reserve identity for a future direct path; they do not
authorize bare guest-memory access, which remains guarded.

A nonzero run request additionally carries the opaque token minted while its
Rust projection lease is live. Native activation validates token ownership,
unique authentication generation, stable descriptor identity, and the exact
active projection while mutation is excluded, then revalidates under every
execution admission. Retiring a token revokes execution but preserves cache
entries for a fresh equivalent lease. A different descriptor, full mapping
replacement, or fork rotates identity and clears chains and the IBTC.
Legacy requests ending before the token field remain guarded. These rules are
the admission prerequisites for the single narrow exception below.

Authenticated AArch64 scalar accesses cover integer literal loads plus
unsigned-immediate, unscaled, and register-offset loads and stores up to eight
bytes. Rust admits direct mode only for a bounded source trace containing an
eligible form and retains the required READ or WRITE projection. Literal
targets are proved statically; dynamic effective addresses are checked for
lower bound, addition overflow, upper bound, and permission before the
descriptor delta is applied. Stores reserve dirty-journal capacity before the
host write and commit the exact dirty range only afterward. Every host access
retains exact guest-PC, access, and width provenance and executes only while the
live fault scope and token admission are published. Vector, prefetch,
writeback, exclusive, overflow, partial coverage, permission failure, stale
tokens, and post-fork requests retain guarded execution or fall back at the
original guest PC.

## Coarse application API

The intended opaque C surface is deliberately small:

```c
hl_native_status hl_native_create(const hl_native_config *, hl_native_executor **);
hl_native_status hl_native_run(hl_native_executor *, hl_native_cpu *,
                               const hl_native_run *, hl_native_exit *);
hl_native_status hl_native_changed(hl_native_executor *,
                                   const hl_native_change *, size_t);
hl_native_status hl_native_before_fork(hl_native_executor *);
hl_native_status hl_native_after_fork(hl_native_executor *, int child);
hl_native_status hl_native_destroy(hl_native_executor *);
```

`run` may execute many reusable blocks. It returns at syscall, signal, fault,
yield, explicit interrupt, or budget boundary. Memory projection and executable
publication capabilities are installed at `create`; no ambient environment,
filesystem, Linux syscall dispatcher, task registry, or application singleton
is visible here.

Every structurally valid public `run` holds a fork-only lifecycle lease until
its outer return. The lease remains held while dispatcher execution admission is
temporarily released for cold trace construction, relocation, and indirect-
branch-cache fill. `before_fork` is nonblocking and returns `STATE` while any
such call exists; after it succeeds, new runs return `STATE` until `after_fork`
repairs the cache and reopens run admission. Ordinary cache mutation continues
to use the narrower execution/mutation gate and is not serialized by this
fork-only lease. Destruction retains its separate externally serialized API
contract.

This matches the retained dispatcher lifecycle in
`../engine/src/core/dispatch.c::run_guest` and the fork repair in
`../engine/src/translator/cache.c::jit_after_fork`: a public dispatcher remains
live across translation, while fork repair treats a peer's in-progress
translation as inherited state that must not be consumed before repair.

## Smallest honest implementation slice

The first runnable slice targets an AArch64 host and imports both retained guest
frontends: AArch64-to-AArch64 and x86-64-to-AArch64. It includes live block
lookup, bounded emission, publication, repeated execution, precise fallback,
and executable-range invalidation. It excludes persistent cache loading,
tier-two promotion, direct inter-block chaining, and fork preservation until
the baseline generation lifecycle is differential-tested. Excluding those is a
performance limitation, not a semantic shortcut: every omitted optimization
returns through the dispatcher or interpreter at an exact architectural PC.

A decoded Rust vector is not accepted as this slice. A single-ISA native path
is also insufficient because the compatibility and nested-engine gates require
both guest ISAs.

## Required proof

Before production selection, each guest ISA needs differential tests against
the retained engine for straight-line arithmetic, conditional and indirect
branches, load/store loops, syscall exit, precise read/write/execute faults,
interrupt exit, executable rewrite, and mapping replacement. A reuse test must
show one translation followed by at least two executions with no additional
translation. Fork, checkpoint, and concurrency tests follow before chaining.

Performance evidence records guest instructions, translated blocks, cache hits,
fallbacks, and wall time. `bigarr`, core workload, DBT, and soak keep their
existing semantics and timeout budgets; a timeout increase is not evidence.

## Historical Rust dispatch ownership and retained fault seam

The instance cache accepts a bounded instruction-provenance batch when a
reserved block is committed. Host publication completes first, every host
subrange is associated with its exact guest instruction, and only then does the
block identity become live. The compatibility `publish` entry point records one
block-wide range; imported frontends must use `publish_map` and provide one
record for each faultable instruction boundary.

The former ordinary-C `dispatch/` and `hl_native_prepare` policy were removed
from this candidate kernel. The deleted `hl-execution::DispatchDecision` converted
the native cache's mechanism-only
observation into translate, enter immutable RX code, or retry after a mapping-
epoch mismatch. `TranslationRequest` and `TranslationEmission` own source-range,
capacity, body-offset, and nonempty-provenance admission before publication.
`fault/`
performs only a bounded, read-only provenance lookup. It allocates nothing,
takes no lock, logs nothing, and cannot unwind. Until provenance entries use
atomic publication and an executor thread-admission registry exists, cache
mutation is externally excluded while this lookup runs; installing it directly
as a concurrent POSIX signal or Windows VEH callback would be premature.

The public mapping-change batch is capped at 1024 records and distinguishes
decoded-source overlap invalidation from whole mapping-epoch replacement.
Diagnostics expose per-instance lookup, hit, miss, epoch-rejection,
invalidation, live-block, and generation counters. Source and benchmark tests
own threshold policy; it does not enter the production native ABI or replace
the required pinned-C wall-time and nested-engine measurements.

There is no production caller of this candidate `exec/` tree. The retained C
closure is separately selected through `execution/ffi`; it must not be confused
with this unselected cache ABI. The proposed next seam was a coarse Rust-owned
translation service
made once on a miss: it receives one bounded request, produces native bytes plus
instruction provenance, and asks the ABI shim to publish them. Frontend-private
helper reasons must resolve to an architecture-neutral fully spilled exit before
a future `hl_native_run` returns. No per-instruction, guest-memory, lookup, or
chain callback may cross the language boundary. Reintroducing dispatch policy or
translation admission under this C tree expands the boundary and is prohibited.

## Host fault context matrix

Host-context recovery stays platform-local inside this boundary. Linux/AArch64
walks the bounded `uc_mcontext.__reserved` record chain and requires a complete
FPSIMD record. Darwin/AArch64 reads the pointer-backed `_STRUCT_MCONTEXT64`, whose
NEON record is inline, and rejects a null machine context. Both paths converge
only after capture at `hl_a64_fault_prepare`, which validates the emitted x16
effective-address provenance, exact access width/range, saved-frame alignment,
and x28 CPU identity before committing architectural state. The platform return
adapter then writes only host x0 and PC to the shared spill-free return
trampoline.

This mechanism installs no process signal owner and does not authorize guard
elision. Signal-handler composition, thread attachment, alternate-stack and fork
lifecycle remain application-owned acceptance gates.

## Native fault thread association

`fault/thread.c` supplies fixed native `_Thread_local` publication on Linux.
Its lifecycle maps directly to retained `src/core/dispatch.c:214-229,426-429`
(`run_guest` publication and per-thread attach/detach),
`src/linux_abi/thread.c:1632-1690` (512 KiB TLS altstack allocation,
registration, disable, and release), and
`src/linux_abi/syscall/proc.c:263-300` (child-side re-arm). Unlike that oracle,
the Rust boundary preserves a pre-existing altstack and refuses to overwrite a
later replacement because this engine is embeddable rather than process-global.
Thread attach saves the complete prior `stack_t`, allocates and registers one
512 KiB alternate stack (ordinary allocator alignment exceeds POSIX stack
alignment requirements), and thread detach first verifies that no later owner
replaced it, then restores the saved stack before releasing storage. Entry
callbacks copy only a borrowed `reserved=1` scope; retirement requires equal
scope contents and its exact generation. Fork-child repair clears inherited
borrowed pointers and recursion state before re-arming the surviving thread's
owned stack. The handler-facing prepare operation declines recursion and does
only bounded native provenance and context work.

The ELF object is compiled with initial-exec TLS and its warning-error gate also
audits disassembly for a direct thread-pointer access with no `__tls_get_addr` or
TLSDESC call. Darwin Mach-O thread-local variables can lower through
`tlv_get_addr`; because that call has no async-signal-safety contract, attach
fails with `PLATFORM` there until a supported direct per-thread association
exists. This module installs no signals and owns no global executor.

Application wiring remains deliberately absent: the current executor callback
is per translated entry, while the scheduler exposes no explicit native worker
start/stop lifetime. Allocating an altstack in that callback would violate the
coarse boundary and leak or thrash pooled worker threads.

## Linux fault coordinator

`fault/coordinator.c` owns the Linux process-wide SIGSEGV/SIGBUS disposition.
Its retained-C oracle is `src/linux_abi/signal.c:deliver_guest_fault` and
`deliver_guest_fatal_fault` for classification/decline, the architecture guard
installation in `src/linux_abi/elf.c` and `src/linux_abi/x86.c`,
`src/core/dispatch.c:run_guest` for CPU/thread association, and
`src/linux_abi/syscall/proc.c:fork_child_hooks` for child repair. The retained
engine is a process-owning executable and overwrites dispositions; this
embeddable boundary additionally composes the disposition it found.
The first acquire snapshots and composes the prior dispositions; later acquires
only retain the coordinator, and the final release restores both dispositions.
Unowned faults preserve `SIG_DFL`, `SIG_IGN`, one-argument and `SA_SIGINFO`
handlers, including their masks, restart, nodefer, and reset-hand behavior. The
handler only reads siginfo/ucontext, consults the fixed thread-local publication,
and either returns through its repaired context or chains; it does not allocate,
lock, log, or unwind. `pthread_atfork` serializes coordinator mutation, retains
the process reference count and dispositions in the child, and invalidates the
surviving thread's inherited active publication.

Process acquire/release and thread attach/detach are deliberately separate.
The application acquires once for each process owner and attaches each host
thread that may enter translated code. A thread publishes borrowed entry scopes
only between attach and detach. This prevents executor construction on one
thread from incorrectly installing another thread's alternate stack.

## Direct-memory authority lifetime

`hl-memory::DirectAuthorityLease` is the safe owner of a potential direct guest
projection. Only `MappingCoordinator::project_direct` can construct it. The
lease retains checkpoint admission, the mapping transaction, host projection,
ledger-qualified permissions and generations, plus write rollback state. It
does not enable bare access.

The app may move that lease into `native::DirectAuthority`, which exclusively
borrows its native `Executor` and registers one opaque native token. Native
registration occurs under mutation admission, mints a nonzero monotonic
generation, and rejects a second live token. Fork repair invalidates the token
in both COW copies; unregister consumes stale tokens without clearing a newer
generation. Destroy remains nonblocking and rejects a live token. The raw C
descriptor is a trusted unsafe application boundary; safe Rust cannot construct
or retain it independently of the memory lease.

This maps the retained lifetime split in `src/core/dispatch.c:214-229,395-423`
(one pinned execution generation) and `src/linux_abi/syscall/proc.c:263-300`
(child repair), without importing retained process globals. No cache key,
translation, guard, load, or store observes the token yet.

## AArch64 atomic capability

Native lowering of x86 locked read-modify-write instructions requires Arm
FEAT_LSE. Linux discovers it through `AT_HWCAP/HWCAP_ATOMICS`, macOS through
`hw.optional.armv8_1_atomics`, and Windows through
`PF_ARM_V81_ATOMIC_INSTRUCTIONS_AVAILABLE`. A failed or unavailable probe
leaves the guest operation at an interpreter boundary.
