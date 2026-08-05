# AArch64 atomic and system boundaries

## Rust ownership

`src/aarch64/atomic/` owns the architectural memory-operation family:

- `mod.rs`: memory ordering, operations, values, reservation identity, and the
  consumer-owned atomic-memory port;
- `decode.rs`: ordered access, exclusive load/store, compare-exchange, and LSE
  read-modify-write decoding;
- `execute.rs`: alignment, failure-atomic register updates, reservation clearing,
  and typed memory-port calls;
- `test.rs`: the atomic memory model and instruction-level regressions.

`src/aarch64/system/` owns the architectural system-instruction family:

- `mod.rs`: barrier and register values plus the consumer-owned system port;
- `decode.rs`: hints, barriers, cache maintenance, and MRS/MSR decoding;
- `execute.rs`: local CPU-register projection, counters, barriers, and instruction
  invalidation;
- `test.rs`: system decoding and execution regressions.

The crate root keeps the established public names (`ExclusiveMemory`,
`GuestSystemPort`, `AtomicValue`, and related values) as compatibility aliases.
Owned declarations inside the domains use contextual names such as `Memory`,
`Port`, `Value`, `Register`, `Decoder`, and `Executor`.

## Retained C oracle

The corresponding working implementation was studied read-only in:

- `../engine/src/translator/guest/aarch64/interp.c`:
  `interp_atomic_pointer`, the local exclusive monitor around lines 1423-1435,
  atomic/exclusive execution around lines 1686-2010, and
  `interp_exec_branch_system` around lines 1073-1210;
- `../engine/src/translator/guest/aarch64/translate.c`:
  `emit_casp`, `is_foldable_mem`, `emit_atomic_part`, `try_lse_atomic`, the TLS and
  CPU-model MRS/MSR projections, exact DC ZVA lowering, and SMC invalidation and
  commit exits around lines 3835-4000;
- `../engine/src/translator/guest/aarch64/stubs.c`: NZCV save/restore and block
  boundary state publication.

The C interpreter requires natural alignment for atomic and exclusive accesses,
performs one indivisible host operation, clears the local monitor after every
store-exclusive attempt, uses acquire/release or sequentially consistent host
ordering, and commits architectural destination state only after a successful
guest access. The native AArch64 path additionally rewrites selected exclusive
retry loops to LSE instructions while preserving the status-register result and
falls back when that rewrite would alter guest behavior.

System state is per guest thread. TLS, NZCV, FPCR, and FPSR read/write masks are
architectural values rather than host CPU leakage. Counter reads share the host
monotonic-clock source and report a 1 GHz frequency. CTR_EL0/DCZID_EL0 describe
the engine's model. IC IVAU queues a translated-code invalidation and ISB commits
it; DC ZVA clears exactly the advertised aligned 64-byte block.

## Explicit parity gaps

This refactor changes ownership only and does not claim full C parity. In
particular, Rust's `GuestSystemPort` currently exposes immediate instruction
invalidation and a generic barrier callback but cannot represent the C engine's
two-phase IC-queue/ISB-commit block exit. Rust atomic ordering is only as strong
as the concrete `ExclusiveMemory` adapter, and production adapters still require
a differential proof of overlapping-location serialization, mapping replacement,
fork reservation clearing, and pair atomicity. DC ZVA ownership and exact
fault/transaction behavior remain in the adjacent memory executor and must be
included in that differential audit.

## x86 and execution module ownership

The crate root now declares the natural `x86` and `execution` nouns instead of
injecting their children into a flat namespace with `#[path]`. `x86/mod.rs`
owns the architectural decoder, scalar IR, CPU/flags state, interpreter and its
integer, MMX, vector, string and x87 collaborators. `execution/mod.rs` owns the
architecture-neutral machine snapshot, private snapshot codec, block-stepping
runner and their tests. The crate root deliberately re-exports the same public
names as before, so this is an ownership migration rather than a public API or
instruction-semantics change. Internal x86 calls now name their actual sibling
owner (`x86::vector`, `x86::scalar`, `x86::fxsave`, and so on), and the private
snapshot codec no longer carries a redundant `Execution` prefix inside the
`execution` noun.

`execution::dispatch` owns the architecture-neutral cache observation and
translation-admission policy that previously lived in transitional
`src/native/execution/src/dispatch/dispatch.c`. A mechanism adapter may report
only missing, available immutable identity, or mapping-epoch mismatch. Rust
selects translate/enter/retry and validates that a frontend's nonempty emission,
body offset, provenance count, source identity, and byte count fit the bounded
request before native W^X publication.

The C and assembly implementation under `src/native/exec` is a permanent native
execution mechanism. It owns machine-code entry and return, translated-code
publication and chaining, W^X mutation, host fault-context reconstruction, and
signal-safe and fork-critical repair. Those responsibilities do not migrate into
safe Rust merely because Rust owns the surrounding execution policy.

The Rust interpreter, correctness fallback, and pointer-free execution snapshot
are also permanent production mechanisms. `ExecutionMachine::run_slice`, the
AArch64 and x86 interpreters, `DispatchDecision`, and `ExecutionSnapshot` remain
the safe execution and recovery path. Deleting unused translated-artifact
persistence scaffolding does not delete, weaken, or make temporary any of those
Rust owners.

The retained comparison for this ownership step read
`../engine/src/core/dispatch.c:run_guest`, `run_block`, and `block_return`;
`../engine/src/translator/cache.c` including the emission arena, translation
index, generations, locks, invalidation, retired mappings, and fork repair; both
guest `translate.c` frontends; and x86-64 persistent `cache.c`. C's live oracle
serializes translation under `g_jit_lock`, publishes bytes before block-map
identity, pins an executing generation in its thread registry, invalidates by
decoded source, repairs fork-copied locks/aliases, and releases mappings at
engine teardown. Assembly entry/return, W^X mutation, async fault provenance,
and fork-critical repair remain legitimate native mechanisms; their surrounding
selection, bounds, identity, and lifecycle policy belong here in Rust.

### Retained C x86 mapping

The corresponding working implementation was studied read-only in:

- `../engine/src/translator/guest/x86_64/decode.c` and `decoder.h`: the
  15-byte fetch boundary, prefix/VEX/EVEX, ModRM/SIB, effective-address and
  immediate decoding behind `hl_x86_decode`;
- `../engine/src/translator/guest/x86_64/interp.c` and
  `interp_dispatch.h`: single-instruction architectural execution, precise
  memory faults, atomic operations, REP/string behavior, vector/x87 fallback,
  block exits and syscall/trap publication;
- `../engine/src/translator/guest/x86_64/translate.c`, `emit.c`, the
  `lower/` family and `translit/`: direct host-code emission, lazy flag
  materialization, block boundaries, SMC invalidation and correctness-first
  interpreter exits for instructions not safely lowered;
- `../engine/src/translator/guest/x86_64/cache.c`: x86 translated-cache
  identity, relocation, restore/save, fork repair and wholesale invalidation;
- `../engine/src/core/target/x86_64.c`: `engine_global_init`, `run_loaded`,
  `hl_run_linux_guest`, guest-memory adapters, signal/fault composition,
  executable-alias write observation, task ownership and teardown;
- `../engine/src/runner/main.c`: the thin process entry that delegates to the
  engine API rather than owning execution semantics.

The Rust `x86` noun corresponds only to C's guest architectural frontend:
decoder, typed instruction model and correctness execution. Rust `execution`
corresponds to the safe machine/snapshot/step contract. C cache publication,
machine-code lowering, signal/ucontext entry, executable-memory mutation and
fork-critical repair permanently remain outside this safe module in the C and
assembly native kernel. Loaded-image,
Linux ABI, task and teardown composition belong above this crate rather than in
either facade.

### Explicit x86 parity gaps

This ownership change does not imply translator parity. Rust presently steps a
safe decoded/interpreted instruction stream and retains only a small decoded
AArch64 block cache; it does not replace C's x86 direct-code translator,
same-ISA transliterator, chained block cache, lazy flag dataflow, provenance
map, or persistent translated-cache lifecycle. Its instruction coverage remains
smaller than the C decoder/interpreter/lowering union, especially across
VEX/EVEX, SSE4/crypto, x87 and unusual faulting encodings. The C engine also
coordinates executable-alias stores with SMC invalidation before externally
observable boundaries, reconstructs guest faults from native contexts, and
owns signal-safe/fork repair paths that the safe Rust interpreter does not
represent. Those are functional migration lanes, not responsibilities to hide
inside `x86/mod.rs` during this structural refactor.

## Ptrace stopped-register boundary

`trace_register.rs` owns the ISA-specific Linux `NT_PRSTATUS` register image at
an execution safepoint. `StoppedRegisterImage` supplies an explicit versioned,
pointer-free envelope; `TraceSafepointPort` is the coarse boundary through which
the task/ABI layer may inspect and return a stopped image. It does not expose
live `CpuState` references across domains and does not itself implement ptrace,
waiting, memory access, signals, or syscall dispatch.

The x86-64 codec is the retained C engine's 27-word, 216-byte
`user_regs_struct` order from `ptrace_publish_regs`/`ptrace_apply_regs` in
`../engine/src/linux_abi/syscall/ptrace.c`: r15 through r8 in Linux order,
general registers, `orig_rax`, RIP, fixed CS 0x33, flags, RSP, fixed SS 0x2b,
FS/GS bases, and four zero reserved words. Applying an image intentionally does
not apply `orig_rax`, selectors, or reserved words, matching C. The AArch64
codec is the retained 34-word, 272-byte order: x0..x30, SP, PC, PSTATE/NZCV.
Both codecs require the exact byte length and use explicit little-endian words.

The remaining ptrace path is deliberately above this boundary: Linux request
decoding and errno behavior, task-stop scheduling, GETREGSET/SETREGSET iovec
semantics, stopped-task memory reads/writes, signal injection, options/events,
and merging trace stops into wait status. Production interpreter/JIT safepoint
adapters are not wired yet, so the existence of these codecs is not a claim
that guest ptrace syscalls work.
