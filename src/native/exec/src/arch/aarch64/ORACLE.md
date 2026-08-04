# Retained AArch64 translator extraction

The behavioral source is `/Users/x/dd/engine`. The runnable same-ISA frontend is
not an opcode table that can be copied independently. Its dependency-closed
baseline is:

| Retained source | Lines | Capability | Native owner |
|---|---:|---|---|
| `translator/host/aarch64/asm.{c,h}` | 151 | Explicit-state A64 encoding | `assembler.{c,h}` (imported) |
| `translator/guest/aarch64/translate.c` | 4,397 | fetch, stolen-register rewrite, memory guards, integer/load-store/control emission | executor frontend context (pending) |
| `translator/guest/aarch64/stubs.c` | 602 | prologue/spill, exits, indirect lookup | executor frontend context (pending) |
| `translator/guest/aarch64/cache.c` | 896 | guest-PC lookup/chaining support | existing `cache/`, adapt remaining mechanisms |
| `translator/guest/aarch64/dispatch.h` | 185 | typed reason mapping and control edges | `hl_native_run` dispatcher (pending) |
| `translator/guest/aarch64/abi.h` | 105 | CPU offsets and reason conventions | generated `schema/cpu` plus typed exits |
| `core/dispatch.c` | 430 | run loop, lookup, publication and invalidation ordering | `hl_native_run` and executor-owned cache |

The 6,615-line closure excludes generic guest-fetch, executable mapping guards,
and the CPU definition. Those must be supplied through bounded native requests,
the generated CPU schema, and the existing arena/cache; Linux ABI, logging,
process state, ELF placement policy and application globals must not be imported.

## State conversion

The following retained globals become fields of one executor-owned AArch64
frontend context: non-PIE address transform values; host I8MM/BF16 capability
bits; soft-memory and bus patch ledgers; current emission state; IRQ patch;
tier-two loop patch state; last translated source interval; feature switches;
and translation/cache counters. Patch ledgers receive compile-time bounds and a
failed latch. Guest instruction fetch is one bounded batch supplied before the
native call; it is never an instruction-at-a-time FFI callback.

The retained `g_emit`, `g_cp`, `g_cache`, `g_rw2rx`, `g_jit_lock`, `g_cpu_key`,
`g_nonpie_*`, `g_host_*`, profiler globals and application service tables are
not admissible native state. Cache reservation/publication, W^X transitions,
provenance and mapping epochs route through the owning `hl_native_executor`.
TLS CPU lookup is removed because block entry already receives the CPU record.

## Vertical acceptance slice

The first runnable import must include the complete baseline integer,
load/store, direct and indirect control, call/return and syscall terminators used
by unchanged `abi/recursion` and `abi/deep_recursion`. It must include x18/x28/x30
and SP rewrites, precise memory provenance, full spill before every typed exit,
bounded interrupt polling and source-range invalidation. Anything outside that
coherent slice returns `HL_NATIVE_EXIT_FALLBACK` at the unadvanced guest PC.

The existing `frontend.c` is an earlier classifier prototype and is not the
source for M2. It must not be extended instruction by instruction or selected by
`hl_native_run`; the retained files above remain the implementation oracle.

## Atomic memory dependency closure

The retained atomic family is not safely separable into opcode-sized ports. Its
dependency closure in `translate.c` is `is_casp`/`casp_uses_stolen`/
`emit_casp_mangled` (pair relocation), `emit_a64_soft_exclusive` (exclusive
monitor behavior), `try_lse_atomic` (LSE ordering and result rules), and
`try_inline_outline_atomic` (host-capability policy). A faithful import also
requires CPU reservation state and invalidation across projected writes,
acquire/release barriers, W-status versus X-result width rules, CASP pair
staging, and publication that cannot expose a partially mutated guest access.
The generated CPU schema currently has no exclusive-monitor record. Therefore
exclusive, LSE, and CASP must remain one pending closure; emitting any of them
before that state and projection contract exists would silently weaken Linux
atomic semantics.

The completed bounded fallback closure is AdvSIMD structure memory: multiple,
single-lane, and replicate forms share the retained byte-span decoder and the
same no-offset rewrite, precise projection guard, and immediate/register
post-index rules. Writes to guest base state occur only after the native memory
instruction succeeds.

## Control-flow dependency closure

Direct `B` and `BL` form one closed retained subset: signed imm26 relative to
the current guest PC, with `BL` publishing the unmodified guest `pc + 4` into
stolen x30 before the typed branch exit. Trace stitching, PLT fusion, outline
atomic fusion, cloning, RAS, and cache chaining are optimizations and are not
part of their architectural dependency closure.

The baseline `BR`/`BLR`/`RET` closure uses a register-target typed exit, stages
stolen x18/x28/x30, and preserves BLR-x30 read-before-link ordering. Retained
indirect lookup and RAS are later executor-owned optimization closures and do
not alter the baseline architectural exit.

Baseline `B.cond`, `CBZ`/`CBNZ`, and `TBZ`/`TBNZ` use two patched typed-exit arms,
the exact 19/14-bit displacement rules, width-sensitive native predicates,
stolen-register staging, and AL/NV both-always handling. Exclusive-region deferral, stitch,
tier-two self-loop, and polling-loop yield are retained optimization closures,
not prerequisites for baseline architectural exits. None of these remaining
forms is routed through the prototype frontend.

## Integer-immediate dependency closure

Move-wide is an independent complete architectural family: MOVN, MOVZ, and
MOVK preserve the native 32/64-bit write rules and legal halfword positions;
MOVK first stages an existing stolen destination because it is read-modify-write.
The destination-ZR form discards the result and all forms preserve NZCV.

ADD/SUB immediate retains native ADD/ADDS/SUB/SUBS arithmetic, including the
optional twelve-bit shift and NZCV publication. Rn=31 remains SP; Rd=31 remains
SP when S is clear and ZR when S is set, preserving CMP/CMN aliases. Stolen
sources and destinations are staged independently so aliases remain correct.

Logical immediate ports the architectural N/immr/imms element-length decoder,
reserved-mask rejection, rotation, and register-width replication before any
emission. Native AND/ORR/EOR/ANDS then preserves ZR, W/X, and TST/NZCV behavior
while stolen operands are staged independently.

ADR/ADRP reconstruct signed imm21 from immhi/immlo. ADR adds it to the exact
guest PC; ADRP adds its page delta to the guest PC's 4 KiB page with 64-bit
wrapping. Both materialize host-independent guest addresses, discard Rd=31,
publish stolen destinations, and preserve NZCV. Application-specific rebasing
and the prototype frontend are outside this closure.

## Integer-register dependency closure

ADD/SUB shifted-register retains native ADD/ADDS/SUB/SUBS arithmetic and
LSL/LSR/ASR behavior after validating shift and W-form imm6 legality. Register
31 is ZR for both sources and the destination in this encoding—never SP.
Independent source staging covers aliased and swapped stolen registers; native
flags preserve CMP/CMN, carry, overflow, N, and Z behavior.

Logical shifted-register retains AND/BIC/ORR/ORN/EOR/EON/ANDS/BICS, including
the N inversion bit and LSL/LSR/ASR/ROR. Register 31 is ZR throughout, preserving
MOV/MVN/TST aliases. Independent stolen sources remain valid when swapped;
flag-setting forms publish N/Z and clear C/V.

Bitfield/extract retains UBFM/SBFM/BFM and EXTR after enforcing sf/N and W-form
immediate legality. BFM separately stages the old destination for merging;
EXTR stages both concatenated sources. Native masks, rotations, sign extension,
and W zeroing preserve LSL/LSR/ASR/ROR aliases without changing NZCV.

Multiply-add retains MADD/MSUB and the Ra=ZR MUL/MNEG aliases with native W/X
wrapping. Three stolen inputs use x16, x17, and a saved guest-x9 temporary;
BFM-style destination merging is not involved and NZCV is unchanged. UDIV/SDIV
retain native W/X non-trapping division after independent source staging:
divide-by-zero produces zero, signed MIN/-1 produces MIN, and signed quotients
truncate toward zero. Variable LSLV/LSRV/ASRV/RORV retains native W/X modulo
shift-count behavior after independent source staging, including counts at
width and width+1. CRC instructions require a separate host-capability policy.

Conditional select retains CSEL/CSINC/CSINV/CSNEG and MOV-like CSET/CSETM/
CINC/CINV/CNEG aliases by evaluating guest NZCV natively. Both candidates are
staged independently, all sixteen conditions remain valid, and retained AL/NV
both select the preferred operand. The operations preserve NZCV.

Conditional compare retains register/immediate CCMP/CCMN and all sixteen
conditions. A true predicate performs native subtraction/addition into NZCV; a
false predicate installs the encoded four-bit NZCV literal. Staged stolen
sources are read-only and no form writes a GPR.

## Terminator dependency closure

The retained translator recognizes exactly `svc #0` (`0xd4000001`) as the
syscall terminator. It fully spills state and exits with `R_SYSCALL` at the
current guest PC; retained dispatch performs service and only then advances PC
by four. Therefore native syscall exit records the current PC, never the next.

BRK, HLT, and UDF have no retained translator-side typed exit mapping. A safe
port requires the signal/fault dispatcher contract, precise provenance, guest
signal selection, and restart PC policy; mapping them speculatively to fallback
or fault would change Linux behavior. Interrupt/budget exits likewise belong to
block-entry polling and scheduler policy, not the architectural SVC closure.

## Block coordination

The retained lowerings are coordinated through bounded source fetch and a
transactional scratch buffer. Every current lowering emits one complete reusable
instruction block (prologue, operation, fully spilled typed exit), so baseline
coordination publishes one guest instruction per cache identity with exact
provenance. Unsupported words produce fallback at the unadvanced PC and are
never published. Keys include mapping incarnation and instruction epoch.

Multi-instruction fusion is a later structural seam: emitters must first split
body emission from prologue/exit emission. Concatenating today's complete blocks
would make every instruction after the first typed exit unreachable.

The shared-body trace closure covers every completed straight-line non-memory
family: move-wide, immediate and shifted arithmetic/logical, PC-relative,
bitfield/extract, multiply, divide, variable shift, conditional select, and
conditional compare. Their single-instruction wrappers use the same validated
bodies. Remaining splits are direct/indirect/conditional control and pair/
structure memory. Conditional control needs deferred dual-exit fixups before it
can share one final exit.

Single, pair, and structure memory now participate through a bounded trace guard queue. Hot bodies
enqueue miss patches; after the one normal exit, each guard emits a precise cold
fallback carrying its own PC, EA, and access. Cold ranges receive additional
provenance entries. The trace rejects guard counts that would exceed the global
provenance bound. Pair multi-target staging and structure post-index writeback
remain in their shared bodies after the guarded native access, so a miss cannot
partially publish registers, vectors, SP, or memory.

Direct, indirect, and conditional control are terminal shared bodies. Trace
translation stops at B/BL, BR/BLR/RET, B.cond, CBZ/CBNZ, or TBZ/TBNZ; it emits
the retained typed exit or dual-exit graph and appends no unreachable hot body.
BL/BLR publish the exact low guest link, including BLR-x30 read-before-write.

Production `hl_native_run` still needs the outer bounded dispatcher loop: derive
the source batch from the request, lookup/build by mapping and instruction
epoch, enter the cached body, convert the fully spilled CPU reason to
`hl_native_exit`, decrement block budget, and repeat branch exits until syscall,
fallback, interrupt, or yield. Cache chaining and IBTC/RAS remain optional
optimizations and are not prerequisites for this baseline loop.

## Native fault provenance audit

The precise AArch64 access records were checked against retained
`translator/guest/aarch64/translate.c`: `is_foldable_mem`, `a64_mem_bytes`,
`a64_mem_required`, `a64_fold_mem_offset`, `emit_a64_bus_guard_instruction`,
`fold_mem_scratch`, `emit_fold_mem`, and `emit_atomic_part`. The retained path
materializes register offsets (including UXTW, SXTW, and LSL), immediate and
writeback offsets, and stolen guest registers into a selected live host scratch,
then de-indexes the faultable opcode to that scratch. Its signal reconstruction
shares `fold_mem_scratch` so it reverses the exact register rewrite.

The Rust-owned native slice has the same simpler invariant: scalar single,
literal, pair, ordered load/store, SIMD structure, and DC ZVA bodies all
materialize and project the complete effective address into host x16 before the
native access. Provenance therefore names captured host x16, with displacement
zero except the four DC ZVA `stp` operations at 0, 16, 32, and 48. Each record
covers only its four-byte host opcode and carries exact read/write direction and
total byte width. Stolen base/index/target staging and pre/post writeback happen
outside that range, so async capture never needs to infer the original guest
register assignment from the rewritten opcode.

Exclusive monitor, LSE, and CASP words remain unsupported in this native slice.
The retained dependency closure includes monitor lifetime, acquire/release
ordering, pair staging, status/result aliases, and loop-to-LSE replacement;
publishing access metadata before that closure executes would falsely describe a
fallback as a native access. Their tests consequently require zero provenance.
The trace owns a fixed 64-record publication array: ordinary one-op memory sites
plus cold guards fill it exactly at 32 instructions, while multi-op DC ZVA traces
that would exceed it are rejected transactionally.

## Linux fault-context reconstruction

The allocation-free capture helper was compared with retained
`translator/guest/aarch64/signal.c` (`hl_aarch64_signal_capture`) and
`core/target/aarch64.c` (`sigframe_capture_fault`). The retained engine copies
non-stolen x0..x30, SP, PSTATE/NZCV, and the FPSIMD context from Linux ucontext;
x16/x17/x18/x28/x30 remain authoritative in the CPU record. Its older folded
address path additionally restores dynamically selected scratch registers from
`mscratch` using the same `fold_mem_scratch` algorithm as emission.

This native slice uses a stronger fixed rewrite contract and needs no scratch
replay: all emitted faultable memory opcodes address host x16, while x17/x18 are
temporary, x28 is the CPU owner, and x30 is the native return link. Guest values
for all five remain in `hl_native_aarch64_cpu`; every other GPR, guest SP, NZCV,
v0..v31, FPCR, and FPSR is live in Linux ucontext. Guards restore non-stolen
temporaries before the exact fault provenance range. Native loads do not commit
their destination on a synchronous fault, and pre/post-index forms were
de-indexed so writeback occurs only after the faultable host opcode. Capturing
the remaining host registers therefore reconstructs the architectural pre-op
CPU state.

Capture validates the CPU-owner pointer in host x28, requires a complete
FPSIMD record, accepts only exact four-byte x16-based read/write provenance and
known widths, and commits no state on failure. Unknown, execute, indexed,
constant, malformed, and unsupported atomic provenance fails closed. The helper
does not install a handler, allocate, lock, log, or mutate execution control.

## Fault return frame

The return seam was checked against retained `core/dispatch.c` `run_block` and
`block_return`, `translator/guest/aarch64/signal.c`
`hl_aarch64_signal_resume`, and this slice's `entry.S`, `stub.c`, and
`run_aarch64`. Both engines enter with one CPU-owned host frame: host x19..x30,
q8..q15, and SP are stored at generated offsets before branching to translated
code. Retained signal resume writes x0=CPU and host PC=`block_return`; that path
restores the frame without spilling translated registers.

`hl_native_aarch64_fault_return` is the dedicated equivalent. The signal-safe
preparation helper first validates the exact x16-based access range, captures
the Linux FPSIMD context, reconstructs guest pre-op state, and records typed
FAULT reason/address/access/size. Only after all checks succeed does it rewrite
ucontext x0 and PC to the assembly trampoline. The trampoline branches directly
to the existing host-frame restore, so no generated `spill()` can overwrite the
reconstructed CPU. The dispatcher now reports `cpu->program` as both exact
faulting instruction and next PC rather than the trace-entry PC.

The frame is deliberately non-nestable per CPU: preparation requires a nonzero,
16-byte-aligned saved host SP and captured host x28 equal to that CPU.

### Darwin host context

The Darwin path was compared line-by-line with retained
`src/host/native_context.h` (the macOS/AArch64 `HL_HOST_UC_*` definitions),
`src/translator/guest/aarch64/signal.c`
(`hl_aarch64_signal_capture` and `hl_aarch64_signal_resume`), and
`src/core/target/aarch64.c` (`sigframe_capture_fault`,
`sigframe_resume_dispatch`, and `mach_resolve_fault`). Darwin owns a pointer to
`_STRUCT_MCONTEXT64`: x0..x28 are `__ss.__x`, x29/x30 are the separate
`__fp`/`__lr` fields, SP/PC/PSTATE are `__sp`/`__pc`/`__cpsr`, and complete
v0..v31/FPCR/FPSR state is inline in `__ns`. Unlike Linux, there is no optional
FPSIMD record chain; a null `uc_mcontext` is the malformed/absent machine-state
case and is rejected without changing the output.

`fault_darwin.c` owns only this platform projection and the final x0/PC rewrite.
It feeds the same `hl_a64_fault_prepare` transaction as Linux, preserving the
same stolen-register, exact-width/range, aligned-host-frame, and CPU-owner
checks. `hl_native_fault_scope_prepare_return` additionally requires captured
PC to equal the provenance lookup PC before dispatching it. No signal action,
Mach exception port, alternate stack, TLS association, mapping guard, or
application policy is installed here. This ports the retained context semantics
without importing its global fault ownership.

## Consumer-owned host-fault port

`hl_native_fault_scope_enter/leave` expose an explicit execution lease to the
application's host-fault owner. While held, mutation and fork admission fail and
destroy returns `STATE` without changing the handle while that lease is active;
a retry after leave atomically closes admission and releases storage. `contains`,
`provenance`, and `prepare_return` are bounded lookups and
POD context writes: they allocate nothing, acquire no lock, and install no
process state. Preparation additionally requires the host ucontext PC to equal
the queried cache address, then uses the dedicated return frame above.

The consumer must publish/unpublish the fully initialized scope using its own
signal-safe thread association, and must leave every scope before invoking the
fork protocol. Signal installation, nesting, masks, alternate stacks, exact
prior-handler chaining, and child-side association repair remain application
adapter responsibilities. An audit of `app/hl-engine/native/termination.rs`
found only cooperative HUP/INT/TERM ownership: it has neither `SA_SIGINFO`
ucontext delivery nor synchronous-fault chaining, so it cannot safely absorb
this port. `app/hl-engine/ffi/linux/signal.rs` implements guest Linux signal
behavior and is not a host signal owner. No app adapter is therefore claimed by
this slice; a dedicated composition owner is still required before production
native-fault recovery is enabled.

The internal dispatcher callback seam does not acquire a second scope lease.
Cold translation and cache publication complete first; with the dispatcher's
existing execution lease still pinning code and provenance, it publishes one
borrowed fixed-POD scope immediately before each assembly entry and unpublishes
it immediately after the fully spilled return. Rejected publication releases
the execution lease and returns `STATE` without entering or invalidating the
new cache block. Branch chaining repeats this pair per native entry. Null
callbacks preserve the prior execution path exactly.
The consumer may copy that POD into its synchronous handler association, but
all copies borrow the executor and CPU only until `unpublish`; it must retire
them synchronously and never use them afterward. The Rust owner trait and view
operations are therefore explicitly unsafe rather than implying owned lifetime.
Borrowed callback scopes carry `reserved=1`; bounded query/return preparation
accepts that tag, while public `scope_leave` accepts only owning `reserved=0`
scopes. A callback therefore cannot copy its view and decrement the dispatcher's
lease.
