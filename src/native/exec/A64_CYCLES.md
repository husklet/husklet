# AArch64 guarded cycle audit

This audit records the retained implementation studied before constraining
cyclic native chains. The imported Rust cache at `8512e5e1c` already patched
resolved cycles without qualification; `8c5b1283f` did not introduce cycle
closure. It added the guarded-safety qualification described below. The
read-only oracle revision was
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`.

## Retained implementation

The complete relevant call path is:

- `../engine/src/core/dispatch.c`: `run_guest` owns lookup, translation,
  publication, execution, reason handling, and the JIT-lock ordering around
  cache mutation. A translated return is a settled guest-state boundary.
- `../engine/src/translator/cache.c`: `map_body`, `map_put`, `add_pend3`, and
  `patch_links_to` own guest-PC identity and direct-edge lifetime. Pending
  edges become direct branches only after target publication and instruction
  cache synchronization. Generation reset, source invalidation, and retired
  arena reclamation bound their lifetime.
- `../engine/src/translator/guest/aarch64/stubs.c`:
  `emit_chain_exit_from` emits an existing direct edge or records a pending
  edge. Its normal mode permits cycles. Once self-modifying code is observed,
  it instead routes removable edges through the shared indirect cache.
- `../engine/src/translator/guest/aarch64/translate.c`: `translate_block`
  places `emit_irq_check` at chained-body entry. `emit_selfloop` and
  `tier2_promote` retain polling while folding a hot conditional back-edge.
  The tier-two mutation is single-threaded; ordinary block publication and
  chaining are serialized by the JIT lock.
- `../engine/src/translator/guest/aarch64/dispatch.h`: `G_IBTC_FILL` publishes
  indirect target/body identity. Threaded publication uses an atomic pair;
  single-threaded publication may additionally patch a per-site cache.

The retained state consists of a generation-qualified translation map, a
bounded pending-edge table, resolved executable bodies, an indirect target
cache, per-thread CPU state, and arena generation ownership. Direct edges
carry no independent lifetime: invalidation makes their source unreachable or
restores/removes ingress before reclaiming the target generation. Every
in-cache cycle remains interruptible because its backward or indirect edge
enters a polled block header. Syscalls and faults fully spill before returning
to the dispatcher.

## Native comparison

The Rust-owned native implementation uses executor-owned arena and cache state:

- `src/arch/aarch64/trace.c::trace_build` emits an interrupt-token and budget
  checkpoint at every published chained-body entry.
- `src/arch/aarch64/stub.c::hl_a64_stub_budget_begin` and
  `hl_a64_stub_budget_finish` preserve NZCV, charge the complete trace, and
  fully spill on interrupt or budget exhaustion.
- `src/executor.c::run_aarch64` authenticates mapping and direct-memory
  authority before entry. The execution admission gate prevents cache mutation
  until the translated return is fully spilled.
- `cache/relocation.c` patches resolved direct edges under exclusive cache
  write ownership and restores them during source/target invalidation.

Unqualified cyclic relocation is unsafe because the cache is also used by the
x86 frontend and by synthetic clients whose body entry need not poll. Each
published block therefore carries a monotone `cycle_safe` capability. Only the
AArch64 trace builder sets it, after placing the interrupt and budget guard at
the exact `body_offset` targeted by relocations. A candidate cycle is admitted
only when the source, target, and every entry examined on the resolved path are
qualified. A missing identity, an unsafe entry, or fixed-frontier saturation
retains the original typed dispatcher edge. Acyclic relocation behavior is
unchanged.

This does not weaken partial-result, syscall, fault, cancellation, or errno
semantics: the qualification changes only whether already-published control
edges remain joined and does not cross a typed non-branch exit. Direct-memory
authority and mapping incarnation remain part of cache identity. Invalidation
still restores incoming relocations before retiring their target.

## Validation scope

No performance improvement is attributed to this change. The imported cache
already closed fully resolved AArch64 cycles, including the direct loops used
by `memcpy` and `memcmp`. The new policy can only preserve that existing
closure for guarded-safe graphs or retain a typed exit for a graph that is not
proved safe.

`test/a64_cycles.c` is the focused acceptance test. It proves:

- a fully qualified two-entry cycle closes both edges;
- a mixed qualified/unqualified cycle retains exactly one typed edge;
- a real conditional self-loop runs until exact budget exhaustion without a
  branch boundary and observes an interrupt before executing another member;
- a real two-block cycle has no steady-state branch boundary; and
- invalidating one member restores its incoming edge, incurs one dispatcher
  boundary for reconstruction, and preserves the exact budget result.

These checks validate the safety classification and existing chaining behavior;
they are not benchmark evidence and do not establish a wall-time change.

## Consolidated accounting and cross-entry evidence (2026-08-05)

## AArch64 translated instruction accounting

### Retained oracle

This lane inspected the read-only retained implementation in
`../engine/src/core/dispatch.c` (`run_block`, `block_return`, and `run_guest`),
`../engine/src/translator/guest/aarch64/stubs.c` (`emit_prologue`,
`emit_spill`, and `emit_chain_exit_from`), and
`../engine/src/translator/guest/aarch64/translate.c` (`stitch_cond`,
`emit_irq_check`, and `translate_block`). The retained CPU record owns guest
register state for the dispatcher lifetime. Translated code borrows it during
`run_block`; a public exit spills before `block_return`. Direct chains retain
live registers, forward edges may skip the interrupt header, and every cycle
retains an interrupt poll. Mapping changes retire translations through the JIT
gate. The retained engine has no request instruction budget, so it has no
equivalent per-block completed-instruction store.

The Rust owner is `src/native/exec/src/executor.c::run_aarch64` and the emitted
admission sequence is `src/arch/aarch64/stub.c::{hl_a64_stub_budget_begin,
hl_a64_stub_budget_finish}`. Rust must additionally preserve exact bounded-run
semantics: a block compares and subtracts its complete instruction count before
entry, a rejected block executes nothing, interrupt ordering precedes budget
admission, and fallback correction restores both uncompleted budget and
instruction accounting.

### Mechanism

Before this change every admitted translated block updated both monotonic
counters independently: `budget -= count` and `executed += count`. The second
operation cost a load, add, and store at every direct-chain destination even
though `executed` is not consumed by generated code, the fault handler, or a
signal callback. For a request with immutable initial budget, the invariant is
`executed = request_budget - remaining_budget`. Fallback correction changes
remaining budget by exactly the uncompleted amount, so the same invariant
covers partial fallback and retry.

Generated code now owns only the admission-critical remaining budget. After
the fully spilled native return, while the same request and CPU are still
owned by `run_aarch64`, the dispatcher validates that remaining budget did not
grow beyond the request and derives `executed`. This removes three hot emitted
instructions without changing interrupt polling, budget rejection, register
publication, mapping identity, fault ordering, locks, or teardown. The CPU is
private to the executing request across this interval; no new shared state or
host call is introduced.

### Evidence

The warning-strict focused native executor suite passed 56/56 on exact candidate
tree. The complete `hl-engine --lib` run reached 478 passed, one pre-existing
options-registry count failure, and two ignored; the failing registry assertion
does not execute native code and also exists at the base revision.

Release artifacts were built alone from base and candidate and exercised on CPU
17 with the same static ARM64 guest (SHA-256
`521ecf12e07c68164b1c0c111eab008985a7e81ab39889301ebde182cac0f537`),
`--divisor 1000 --phase compute`, typed native execution plus diagnostics, and
11 repeats. Checksums and all execution counters matched, including 337,290
completed instructions. Base median/min/max was 1,824/1,681/2,277 microseconds;
candidate was 1,724/1,654/2,585 microseconds, a 5.48% median reduction. The
candidate and base engine hashes were respectively
`0450130cba4c1180dc59e93cbf2cacbd0df9dc54ee04b6dd0f852cd04d0567ba`
and `76b8e1c5e632db08767d1503a8257314b4e440606efa14a65b769203dc349739`.
Host load was elevated and the ranges overlap, so this is focused causal
evidence rather than a release performance claim; the exact removal of three
instructions per admitted block is independently source-verifiable.

## AArch64 conditional trace accounting

### Oracle and ownership

This lane inspected the retained implementation in
`../engine/src/core/dispatch.c` (`run_block`, `block_return`, and `run_guest`),
`../engine/src/translator/guest/aarch64/stubs.c` (`emit_prologue`,
`emit_spill`, and `emit_chain_exit_from`), and
`../engine/src/translator/guest/aarch64/translate.c` (`stitch_cond`,
`emit_irq_check`, and `translate_block`). Retained conditional fall-through is
bounded and inline. Taken edges leave through a chain exit. Every in-cache
cycle retains an interrupt poll; forward edges may omit redundant polls. State
is live in host registers until a public exit spills it, and mapping/JIT
generation changes retire every dependent edge.

Rust additionally owns a request instruction budget. `trace.c` and `stub.c`
reserve translated work before execution, `executor.c::run_aarch64` owns the
request-local remaining budget and derives completed work after a fully spilled
return, and cache relocation owns target generation and cycle admission.

### Charged-prefix invariant

A stitched trace reserves its complete fall-through interval once. Each taken
edge refunds the suffix after that conditional before entering its relocation
or spill. Therefore:

```text
request budget - remaining budget = guest instructions completed before exit
```

The unsupported instruction at a fallback boundary belongs to the translation
source interval for provenance and invalidation, but is not completed work.
`instruction_count` records only the supported prefix; `source_last` continues
to cover the fallback instruction. This distinction was previously hidden
because a conditional ended the trace before the later unsupported word.

The entry interrupt and token checks remain before the single reservation.
Stitching is bounded to three conditions and 32 guest words. A backward or
relocated cycle re-enters the destination admission and polls again. An inline
fall-through performs no extra poll, matching the retained bounded-region
contract. Exclusive load/store intervals are never crossed.

### Diagnostic deltas

`completed` is explicitly native-only accounting: it is incremented after
`hl_native_aarch64_enter`, while interpreter work is not included. A stitched
trace can execute a supported prefix natively before reaching an unsupported
instruction which, without stitching, was encountered at index zero of a new
block and handled through fallback. Thus native completed work, builds, cache
hits, public branches, and fallback counts may change even with identical guest
architectural work.

On the fixed compute workload the unstiched tree reported 337,292 native
instructions, eight branch boundaries, and two fallback boundaries. The
candidate reported 349,590 native instructions, four branch boundaries, and
three fallback boundaries. Direct charged-prefix tests prove that the 12,298
additional native instructions are newly covered prefixes, not missing taken
refunds. Checksums and final per-request budget remain the acceptance signals;
these structural diagnostics are expected attribution evidence.

### Required evidence

The direct trace cases cover taken and fall-through paths, a nested call and
conditional path, constrained-budget yield, relocation, and a 100-iteration
backedge with exact `executed == 301` and zero remaining budget. Existing
asynchronous-token, forward-chain, cycle, invalidation, and fallback tests must
remain warning-strict and green. Performance acceptance additionally requires
paired same-CPU base/candidate runs with identical checksum and native-verified
mode.

## AArch64 cross-entry metadata audit

### Retained oracle

The read-only audit covered `../engine/src/translator/cache.c` (`map_set`,
`map_invalidate_source_ranges`, pending-link publication and generation
retirement), `../engine/src/translator/guest/aarch64/translate.c`
(`emit_irq_check`, `emit_bl_ras`, direct/conditional stitching and SMC ingress
removal), `../engine/src/translator/guest/aarch64/stubs.c` (`emit_prologue`,
direct/indirect chain ingress), and `../engine/src/core/dispatch.c`
(`run_guest`, `run_block`, `block_return`). The global JIT lock owns retained
map/link mutation; translated bytes remain immutable until generation
retirement. Direct forward edges may skip the retained IRQ header, while cycles
retain an IRQ ingress. Invalidation removes map, IBTC, pending-link, and shadow
ingress before reclamation. Host branches change W^X and signal mechanics, not
entry identity.

### Rust owner and capability matrix

The corresponding owners are `src/arch/aarch64/trace.c` for exact trace count
and admission layout, `src/translation.c` for emission publication,
`cache/cache.c` for cache-entry identity/lifetime, and `cache/relocation.c` for
cold and warm edge resolution, invalidation, cycle qualification, and teardown.
Mutation admission serializes publication and patching; generation, mapping,
instruction, memory-mode, and authority identities qualify every live entry.

| Capability | Retained | Rust before this change | Status |
|---|---|---|---|
| Guarded ingress | map host entry | `body_offset` | implemented |
| Post-admission ingress identity | implicit address after IRQ header | not retained | implemented as cache-owned `admitted` identity |
| Exact destination count at edge resolution | not needed by retained IRQ-only admission | available transiently on target entry | implemented in resolved relocation metadata |
| Cross-entry reservation | forward edges can skip IRQ only | all edges target guarded body | intentionally missing |
| Cycle progress | cycle keeps IRQ ingress | `cycle_safe` graph qualification | unchanged |

Trace formation records the post interrupt/token/budget address, publication
turns it into an arena-relative cache-entry identity, and lookup/execution
identity returns it with the existing guarded body. Relocation resolution copies
the target's nonzero exact instruction count into the resolved edge; returning a
resolved edge to the cold queue clears the count because the replacement target
may have a different trace shape. No patch target uses either field yet, so
execution, partial-result, cancellation, signal, errno, invalidation, locking,
and teardown behavior are unchanged.

## AArch64 cross-entry reservation ownership

### Retained oracle

This isolated lane inspected the read-only retained implementation in
`../engine/src/translator/guest/aarch64/stubs.c` (`emit_prologue`,
`emit_spill`, `emit_chain_exit_from`, and the indirect-branch probes),
`../engine/src/translator/guest/aarch64/translate.c` (`emit_irq_check`,
`stitch_cond`, and `translate_block`), `../engine/src/translator/cache.c`
(`add_pend3`, `patch_links_to`, and `map_invalidate_source_ranges`), and
`../engine/src/core/dispatch.c` (`run_block`, `block_return`, and
`run_guest`).

The retained CPU record owns architectural state for the dispatcher lifetime.
Generated code borrows live registers until a public exit spills them. The
global JIT lock serializes map, pending-edge, and executable-byte mutation;
publication completes before a pending edge becomes reachable. A resolved
direct edge is one patched branch. Forward edges may skip the retained
interrupt header, while backward edges and cycles retain a poll. Invalidation
removes map and indirect ingress before the immutable generation is retired.
Host-specific branches alter executable publication and signal entry, not the
guest-PC identity or edge semantics. The retained engine has no bounded request
budget or asynchronous request token, so its forward-poll elision is not by
itself an oracle for Husklet admission ordering.

### Rust owner and comparison

The corresponding Rust-owned native sources are
`src/arch/aarch64/{direct.c,conditional.c,stub.c,trace.c}` for edge emission and
interrupt/token/budget admission, `src/translation.c` for cache publication,
and `cache/{cache.c,relocation.c}` for entry identity, pending/resolved edge
ownership, W^X patching, invalidation, and teardown. The executor-private cache
and CPU live for one native execution owner; cache mutation is admitted before
the arena writable alias is used, and resolved relocations are returned to a
typed cold exit before a removed target can be reclaimed.

The prerequisite metadata now has an owned, behavior-preserving reservation:

| Capability | Rust state | Result |
|---|---|---|
| Post-admission target identity | `cache_entry.admitted_offset` | implemented |
| Exact destination charge | `resolved_relocation.relocation.target_instruction_count` | implemented |
| Cold-edge reversibility | `expected` plus resolved-to-pending invalidation | implemented for one word |
| Edge-local admission program | sixteen-word typed cold span before every AArch64 chain exit | reserved, not yet consumed |
| Atomic multiword patch/restore | cache validates, publishes, and restores the complete span | implemented |

`hl_a64_stub_budget_begin` orders interrupt, asynchronous-token, and budget
checks before subtracting the complete block charge. Its `admitted_offset`
follows all of those checks. Branching there is correct only after the edge has
performed the same admission transaction. Subtracting the recorded target
count without first checking the budget can underflow; checking only the budget
can delay an already-visible interrupt or token indefinitely across an acyclic
chain; branching to `body_offset` merely repeats the existing full guard and
does not consume either new metadata field.

Direct, conditional fall, conditional taken, and stitched taken exits reserve
sixteen NOP words before their unchanged cold dispatcher exit. The relocation
owns that exact cold image. `patch()` validates the full image, replaces only
its first word with the existing direct branch, and publishes the full range;
the unused words remain unreachable on a hot edge and execute as NOPs on a cold
edge. Invalidation and dynamic site rebinding validate the patched first word
plus every cold tail word, restore the complete image, and publish the complete
range before ownership changes. This preserves behavior until a later lane
consumes the reservation with the admission program.

### Delivered bounded mechanism

`hl_native_relocation_span` is fixed-size and allocation-free. Legacy producers
with a zero span count retain the one-word contract; AArch64 trace emission
always supplies the typed sixteen-word form, with a compile-time equality check
between emitter and cache capacities. Existing cycle admission decides whether
the reservation remains cold before any mutation. Pending cold and wildcard
edges retain the same value-owned image, resolved invalidation returns it to
pending ownership, and cache/fork reset drops the generation containing both
pending and resolved values.

`test/cache.c::relocation_span` exercises cold wildcard ownership, cold-to-hot
resolution, preservation of the unused tail, complete invalidation restore, and
reset retirement. Existing AArch64 trace and cycle tests continue to cover both
conditional successors, direct edges, cycle retention, capacity paths, and
source/target invalidation through the same relocation implementation. The
### Reservation consumer

The follow-on consumer re-read the retained files and entry points listed above,
including the complete `translate_block` stitching path, dispatcher return path,
pending-link publication, source-range invalidation, and teardown. The retained
ownership and host/architecture conclusions are unchanged: live registers cross
patched edges under the global JIT mutation lock, invalidation restores cold
ingress before target retirement, and the retained engine has neither Husklet's
request token nor exact bounded-run accounting.

Rust now replaces a typed sixteen-word span with one indivisible admission
program. It polls `interrupt`, acquire-loads and polls the optional request
token, loads the destination budget, preserves NZCV, rejects insufficient
budget, precharges the target's exact instruction count, publishes the new
remaining budget, and branches to `cache_entry.admitted_offset`. Interrupt and
token rejection enter the unchanged cold spill directly; budget rejection first
restores NZCV. The resolved relocation owns the complete hot image, so dynamic
rebinding and target/source invalidation validate all sixteen words and restore
the complete cold image before changing ownership.

The audit exposed one prerequisite handoff gap: `trace_build` computed the exact
count but `hl_a64_trace_cache_direct` omitted it from `hl_native_emission`, causing
cache publication to use the compatibility default of one. Publication now
carries the computed count. This is required for conditional suffix refunds:
destination precharge minus the already-completed-prefix refund equals exactly
the newly executed guest work.

`test/cache.c::relocation_span` checks the exact instruction layout, destination
count immediate, admitted ingress branch, NZCV restoration, and complete
invalidation rollback. The warning-strict C test passes. On the pinned AArch64
host, the ten-million conditional branch budget test and asynchronous-token loop
test each pass in 0.03 seconds; the package's 505-test library cohort passes all
AArch64 admission tests. One unrelated network timing test returned `WouldBlock`
in the full concurrent cohort and passed alone in 0.03 seconds.

## AArch64 cold execution-identity publication

### Retained oracle and ownership

The read-only retained audit covered `../engine/src/core/dispatch.c`
(`run_guest`, `run_block`, and `block_return`),
`../engine/src/translator/guest/aarch64/translate.c` (`translate_block`,
`stitch_cond`, and `emit_irq_check`),
`../engine/src/translator/guest/aarch64/stubs.c`
(`emit_prologue`, `emit_spill`, and `emit_chain_exit_from`), and
`../engine/src/translator/cache.c` (`map_host`, `map_put`,
`patch_links_to`, `jit_publish_code`, and `jit_flush_to_fresh`). The retained
dispatcher owns one CPU for the guest thread lifetime. Generated code borrows
it while architectural registers remain live; every public exit spills before
return. The JIT lock serializes threaded emission, code is published before
map, pending-chain, or IBTC ingress, and retired generations remain immutable
until no executing thread pins them. AArch64 host signal capture reconstructs
state from host PC plus published provenance; non-AArch64 hosts do not enter
this code. Linux and Darwin differ in ucontext and W^X mechanisms, not in trace
admission.

Retained bounded traces inline fresh conditional fall-through successors, poll
every in-cache cycle, and deliver pending signals only after a settled spill.
The retained engine has no request instruction budget and does not publish a
per-entry cache identity for partial-budget correction.

Rust adds request-local budget accounting and projected-memory retry. Its
execution gate pins the cache, authority, provenance, and CPU from lookup
through fully spilled return. `trace.c::trace_build` emits interrupt-token and
whole-trace budget admission. `executor.c::run_aarch64` consumes a bit-one
`cpu.indirect_site` identity only when a projected-memory guard fallback must
identify the executed trace and refund its uncompleted suffix. Host fault and
signal callbacks use host-PC provenance and never consume this field. The
aligned form remains separately owned by `indirect.c` for IBTC miss patching.

### Capability matrix

| Capability | Retained C | Rust owner | Status |
|---|---|---|---|
| Registers live across direct edges; spill on public exit | dispatch assembly and AArch64 stubs | `entry.S`, `stub.c` | implemented |
| Bounded conditional fall-through stitching | `translate_block`, `stitch_cond` | `trace.c`, `conditional.c` | implemented |
| Interrupt poll on every cycle | IRQSLIM admission and chain emission | budget header plus relocation `cycle_safe` closure | implemented |
| Whole-request instruction budget and exact suffix refund | no corresponding budget | `stub.c`, `trace.c`, `run_aarch64` | Rust-only, implemented |
| Publish code before executable ingress | JIT lock, publish, map/patch/IBTC order | cache execution gate and atomic provenance publication | implemented |
| Async fault reconstruction without dispatcher state | host PC plus cache provenance | fault scope plus atomic provenance | implemented |
| Identify the executed trace for operand retry | not required | bit-one `indirect_site` plus `hl_native_cache_execution` | implemented, previously over-eager |
| Per-site indirect branch patch identity | retained IBTC site | aligned `indirect_site` in `indirect.c` | implemented and unchanged |

### Mechanism

Previously every dispatcher entry, direct chain, and IBTC hit executed `ADR`,
`ADD #1`, and `STR` before interrupt and budget admission merely in case that
trace later took a projected-memory guard fallback. Ordinary branch, syscall,
yield, interrupt, and successful memory paths never consume the value.

The guard cold stub now publishes its own in-entry address tagged in bit zero
immediately before its fully spilled fallback exit. Any address within the
pinned entry identifies the same immutable cache record. Normal trace entry no
longer performs the three instructions. This preserves local and token
interrupt ordering, reject-before-work budget semantics, conditional refunds,
fault provenance, W^X publication, cycle qualification, and IBTC patching. No
lock, allocation, host call, or destructor is introduced into generated code
or a signal callback.

### Evidence contract

Focused native executor tests cover asynchronous token interruption, bounded
budget returns, warm direct/indirect chains, projected-memory retry, fault-owner
lifetime, cache reset, fork repair, and executable-write invalidation. Warning-
strict build evidence must be recorded from the exact committed tree. Pinned
performance evidence uses one diagnostics-on proof followed by diagnostics-off
timed rows through the typed benchmark matrix; ambient engine options are not
evidence.

On Linux AArch64 CPU 17, the harness first proved seven native runs and then
timed seven quiet rows from harness commit `9fcaf4f1c`. Baseline engine SHA-256
`f05bdb8b1fea904cda27a738d91f2fa64a9df3808954476a35a59c807ef1c774`
had a 1,948 us median; candidate SHA-256
`37595c1a7e40d11e567b22c1b130650225f09d6bab911200bc86ea1bf8fd682d`
had a 1,841 us median, a 5.49% reduction. Both used guest SHA-256
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`
and retained runner SHA-256
`0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62`.
Every row returned checksum `9349119015121845085`; both diagnostic proofs
reported seven runs, six builds, 18 hits, four branch exits, one fallback,
five yields, 349,589 completed instructions, and one guard fallback. Admission
load was elevated (13.29 for baseline and 11.55 for candidate), so the timing
is focused causal evidence rather than a release claim. The exact removal of
three hot AArch64 instructions per admitted trace is source-verifiable.
