# AMD64 REP quantum audit

> Historical replacement-engine audit. Rust/native benchmark commands and
> verdict thresholds below are retained as migration evidence, not as current
> product instructions. The C-primary campaign is described in
> [`../README.md`](../README.md).

## Exact-quantum completion boundary

The follow-up audit used detached commit `d6d1ad382`. In addition to the
retained files below, it inspected
`lower/repstr.c::emit_rep_string` through its completed-count epilogue,
`interp.c`'s MOVS/STOS cases, and `dispatch.h::G_DISPATCH_REASON`. The retained
owner keeps one thread's spilled CPU live across helper completion, applies
completed elements to RCX/RSI/RDI before executable-write draining, and only
then resumes the following instruction. Faults retain the REP RIP and exact
partial registers; DF and widths 1/2/4/8 share that ownership model. There is
no REP-local lock or teardown object: pins and helper state are released on
every return, while dispatcher/thread registration owns signal and shutdown
lifetime. Host-specific bulk lowering is AArch64-only; the retained
interpreter is the elementwise fallback on other hosts.

The active comparison covered `run.c::{rep_execute,hl_native_x86_64_run}`,
`executor.rs::{run_x86_inner,poll_quantum}`,
`scheduler.rs::{advance,native_x86}`, mapping `ProjectionLease` request and
checkpoint continuations, and the generation-qualified scheduler
continuation. The completed REP path has already advanced RIP, charged every
element, updated RCX/RSI/RDI, and published exact dirty ranges before returning
to the run loop. It is therefore the same safe polling boundary as incomplete
REP exhaustion. A successful poll adds exactly `quantum_grant` to the explicit
cumulative admission before the following instruction enters; denial still
produces the ordinary public yield. Interrupt, signal/cancellation, scheduler
generation, mapping request, checkpoint request, and overflow checks are
unchanged.

| Capability | Retained C | Active owner after this change |
|---|---|---|
| widths 1/2/4/8 and DF | helper plus scalar fallback | `rep_decode`, `rep_copy`, `rep_fill` |
| partial fault progress | completed-count epilogue | scalar fallback after bounded bulk prefix |
| exact accounting | no equivalent request budget | per-element `executed`, cumulative grants |
| incomplete quantum boundary | dispatcher/helper state is coherent | grant poll already implemented |
| exact-quantum completion | resumes next instruction in dispatcher | completed REP is grant-eligible |
| signal/cancellation/mapping/checkpoint | dispatcher registries and pins | unchanged continuation tokens |

The structural regression uses REP MOVSB ending exactly on budget two followed
by SYSCALL. It proves one poll observes `(executed, admitted) == (2, 2)`, the
syscall enters under the two-credit grant, and the result is exactly three
executed with one remaining credit. The existing `x86_rep` cohort covers all
MOVS/STOS widths, both directions, overlaps, permission faults, split views,
zero count, dirty publication, and partial progress.

## Scope and evidence

This is a read-only design audit for eliminating repeated native `YIELD`
round trips around AMD64 `REP MOVS` and `REP STOS`. It does not authorize an
application-specific path, a changed instruction-accounting definition, or a
larger unobserved scheduling interval.

The accepted exact-clean measurement used detached Husklet commit
`5b0d27e85`. Five AMD64 memory rows were `native-verified`, retained checksum
36,526, and reported a 119,295-us median with a 118,386--138,524-us range.
Every row reported 7,131 x86 public exits, 238 builds, 397,962 hits, and five
fallbacks. The prior Rust checkpoint median was 303,537 us and the retained-C
median was 6,531 us. Those values establish a remaining gap; they do not prove
that public exits account for all of it, and this document makes no speed
claim for an unbuilt continuation mechanism.

The benchmark invokes `phase_memory` with 400 measured iterations because the
declared 8,000 iterations are divided by 20. Its two warmups each execute
`400 / 20 + 1`, or 21, iterations. The command therefore performs 442
one-MiB copies. A one-byte `REP MOVS` consumes sixteen 65,536-element quanta
per MiB, predicting 7,072 budget exits. That accounts for 99.17% of the 7,131
observed public exits and leaves 59 other exits. Site admission and the phase's
initial fills mean the diagnostic must be measured again after any change;
7,072 is a source-backed attribution for this recorded run, not a promise that
the next run will contain exactly 59 exits.

## Retained-C oracle

The following retained files and entry points were studied without editing
`../engine`:

- `../engine/src/core/dispatch.c::run_guest` owns a guest thread's `cpu`
  lifetime. It registers the thread with signal and stop-the-world registries,
  clears and observes `irq` at fully spilled dispatcher boundaries, admits a
  checkpoint safepoint, resolves or translates the next block, enters it, and
  handles its reason before signal delivery. Cache and registry locks do not
  remain held across a guest syscall.
- `../engine/src/translator/guest/x86_64/lower/repstr.c::hl_x86_lower_repstr`
  recognizes the complete MOVS/STOS width and direction family.
  `emit_rep_string` spills registers, vectors, and flags before its host call;
  passes RCX, RSI, RDI, width, DF, CPU, and faulting RIP by value; applies the
  exact completed-element count to RCX/RSI/RDI; drains executable-write
  observation only after that architectural state is coherent; and reloads
  the guest state.
- `../engine/src/translator/guest/x86_64/rep_runtime.c::hl_x86_rep_movs` and
  `hl_x86_rep_stos` validate the complete range and select whole-span libc
  copying or filling when it is safe. `rep_movs_pinned` and
  `rep_stos_pinned` acquire the largest contiguous source and destination
  pins, release every pin on success and failure, and publish each completed
  store span. `rep_movs_scalar` and `rep_stos_scalar` preserve partial-fault
  ordering one element at a time. Forward overlap smears in architectural
  order, backward DF walks high to low, and a rejection leaves the faulting
  element uncommitted.
- `../engine/src/translator/guest/x86_64/dispatch.h::G_DISPATCH_REASON`
  resumes helper and fallback reasons inside the same per-thread dispatcher
  and exposes only real syscall, signal, fault, stop, and exit boundaries.

The retained engine performs an admitted REP span without an instruction
budget poll. It is the throughput and Linux-behavior oracle, but its coarser
latency does not justify removing the Rust engine's explicit scheduling,
signal, cancellation, mapping, or checkpoint bound.

## Current Rust-owned path

The following active owners were compared:

- `src/containers/hl-engine/src/ffi/linux/execution/scheduler.rs`:
  `GuestExecutor::schedule`, `advance`, `execute_turn`, `native_slice`,
  `native_x86`, `apply_turn`, `native_budget`, and `charge_elapsed`.
- `src/containers/hl-engine/src/ffi/linux/execution/threads.rs`:
  `ThreadSet::next`, `is_only_runnable`, `release`, process-control gates, and
  generation-qualified `ThreadRun` ownership. `ThreadSet::charge_cpu` delegates
  to the lock-taking task registry.
- `src/runtime/hl-task/src/registry/state.rs::TaskRegistry::charge_cpu`, which
  updates process usage while holding the registry state lock.
- `src/runtime/hl-runtime/src/process/itimer.rs::poll_cpu_itimers` and
  `src/runtime/hl-runtime/src/signal/boundary.rs::deliver_signal_boundary`,
  which read the CPU clocks, poll the alarm owner, and enqueue or deliver the
  resulting signal through lock-taking runtime and task-domain paths.
- `src/containers/hl-engine/src/ffi/linux/execution/space.rs`:
  `AddressSpace::with_execution_memory` and the current-image lease.
- `src/runtime/hl-memory/src/mapping/projection.rs`:
  `Coordinator::project_contiguous`, `ProjectionLease`, additional bounded
  projections, exact dirty publication, rollback, and drop.
- `src/runtime/hl-memory/src/checkpoint_activity.rs`:
  `CheckpointActivity::{admit_memory,freeze,thaw}` and
  `ActivityAdmission::drop`.
- `src/containers/hl-engine/src/native/executor.rs`:
  `Executor::{run_x86_lease,run_x86_inner}`, the `NativeX86` capture, restore,
  and write-publication methods, the operand resolver, and source/view hint
  lifetimes.
- `src/runtime/native/exec/src/arch/x86_64/run.c`:
  `rep_decode`, `rep_view`, `rep_span`, `rep_execute`,
  `hl_native_x86_64_run`, and `leave_exit`.
- `src/runtime/native/exec/include/executor.h::hl_native_run_request`, whose `budget`
  is explicitly the maximum guest-instruction count for one activation.

One current quantum has this order:

1. The scheduler owns an exact `(thread, process, generation)` run in the
   `Running` state and selects 65,536 credits only when every other machine is
   parked. Shared-runnable execution receives 4,096 credits.
2. `with_execution_memory` pins the current image. `native_x86` snapshots the
   mapping generation and executable token, reads the source, and creates the
   primary and hinted operand projections.
3. `ProjectionLease` holds checkpoint activity admission, the mapping
   transaction mutex, host projections, and write reservations. None of its
   raw host pointers may outlive the lease.
4. `run_x86_inner` copies the architectural CPU into the native record and
   enters C. `rep_execute` preflights permissions, contiguous spans, overlap,
   dirty capacity, and arithmetic before changing bytes.
5. Each completed element advances RCX/RSI/RDI according to DF, increments
   `executed`, and consumes one credit. Partial completion retains the REP RIP;
   complete execution advances it.
6. Exhaustion calls `leave_exit`, leaves native execution with all state
   spilled, and returns a public `YIELD`.
7. Rust restores the CPU, converts native dirty records to exact guest ranges,
   commits those ranges, and drops the projection lease. Only now can a
   mapping transition or checkpoint obtain its authority.
8. The scheduler converts the yield to a continuation, releases run ownership,
   charges elapsed thread CPU, rechecks cancellation and completions, selects
   the next runnable generation, and repeats.

This ordering explains the cost: every 65,536 elements reconstructs source and
projection state, crosses the public native ABI, publishes writes, releases
the run, and re-enters the scheduler.

## Rejected shortcuts

Charging a complete REP chunk as one instruction is rejected. It changes the
shared interpreter/native accounting contract, lets budget one enter as many
as 1,048,576 byte operations, and delays all existing boundaries by the same
factor.

Increasing `hl_native_run_request::budget` or silently refilling it in C is
also rejected. The request documents a hard maximum, x86 currently snapshots
interrupt state at entry, and the projection lease would retain checkpoint,
mapping, pointer, and write authority across a boundary invisible to the
scheduler.

Immediately resuming after a public yield in `GuestExecutor` can preserve
semantics if it first performs all ordinary checks and lets the projection
lease publish and drop. It cannot remove the 7,072 native exits or their
projection/ABI work, so it is at most a separately measured precursor.

A callback that checks only `InterruptToken` is insufficient. Checkpoint
freeze and a mapping transition waiting for the transaction mutex are not
represented by that bit. Querying `ThreadSet` from C while the projection
transaction is held is also unjustified until the lock order is explicit.

## Required architecture

The coherent mechanism is an optional consumer-owned, typed quantum poll. It
is borrowed by one synchronous native activation and is never retained. Native
x86 invokes it only after a fully spilled 65,536-instruction boundary. The
poll either grants exactly one further bounded quantum or denies continuation,
which produces the ordinary public yield and existing publication sequence.
Every grant extends an explicit cumulative admitted budget; `executed` retains
its per-element meaning and may never exceed the sum of granted credits.

The following prerequisites are load-bearing:

1. Memory needs a lock-free request epoch published before checkpoint freeze
   waits for admissions and before every mapping transition waits for the
   transaction mutex. A projection captures that epoch and can answer whether
   continuation is still current. Exposing only
   `CheckpointActivity::frozen()` does not reveal a queued mapping change.
2. The scheduler needs a generation-qualified atomic continuation token. It
   is invalidated when another thread becomes runnable, this run is retired or
   replaced, process control changes, a deliverable signal or interrupt
   appears, or cancellation is requested. Shared-runnable execution never
   grants a refill.
3. CPU accounting and interval timers need one of two explicit policies. The
   complete policy is task/time-owned lock-free publication: an atomic usage
   accumulator plus an atomic timer armed/deadline/generation snapshot lets
   the poll account elapsed host thread CPU and detect expiry without entering
   `TaskRegistry::charge_cpu` or `poll_cpu_itimers`. Expiry denies the grant;
   the ordinary boundary publishes the signal after the projection drops. The
   conservative policy denies every refill whenever a CPU timer is armed or
   the process has any accounting-sensitive boundary. It retains the existing
   lock-taking charge and timer poll after the public return. Timer arming must
   invalidate the continuation token before publishing the new timer state;
   disarming must not revive an activation whose captured timer generation is
   stale. A fresh scheduler boundary may admit a later activation.
4. The poll uses atomic state only while `ProjectionLease` holds the mapping
   transaction. It must not acquire the thread-set, checkpoint-activity, or
   mapping locks in an unreviewed order. In particular, directly calling
   `charge_elapsed` is forbidden here because it reaches the task-registry
   lock, and directly polling CPU interval timers is forbidden because it
   reaches the alarm and task domains. Denial returns through normal Rust
   unwinding, publishes or rolls back writes, and releases all authority
   before lock-taking accounting, signal, checkpoint, control, or mapping work
   begins.
5. Fault, executable-write epoch, operand-resolution decline, callback error,
   and callback panic all fail closed. No callback may unwind across FFI.

These states are not currently exposed. In particular, the mapping transaction
is a plain mutex with no waiter/request generation, and CPU usage plus timer
polling remain behind task/runtime locks with no atomic armed/deadline
publication. Implementing only the native callback would therefore weaken
required semantics and is blocked until the memory, task/time, and scheduler
prerequisites land as coherent domain changes.

## 2026-08-05 prerequisite re-audit at `cf15cdd33`

Mechanical inspection of the pinned tree shows that one prerequisite has
landed since the design above was written, but the complete mechanism remains
blocked. `hl-memory::CheckpointActivity` now owns an `AtomicU64` request epoch;
`freeze`, `begin_exit`, and `terminate` invalidate it before waiting, and an
`ActivityAdmission` supplies a lock-free `CheckpointContinuation`. The focused
tests cover freeze-before-wait ordering, exit/termination, and saturated epoch
fail-closed behavior.

| Required capability | Exact owner inspected | State at `cf15cdd33` |
| --- | --- | --- |
| Checkpoint request invalidates an admitted run before waiting | `src/runtime/hl-memory/src/checkpoint_activity.rs::{CheckpointActivity,ActivityAdmission,CheckpointContinuation}` | implemented |
| Mapping request invalidates before waiting for the projection transaction | `src/runtime/hl-memory/src/mapping/{host,projection,change,external,remap,checkpoint}.rs` | missing; all mutation paths still acquire the plain `AddressSpace::transaction` mutex without a published request generation |
| Only-runnable, generation-qualified scheduler continuation | `src/containers/hl-engine/src/ffi/linux/execution/{scheduler,threads}.rs` | missing; run generation is lock-owned and no atomic grant token spans a native activation |
| Signal, cancellation, control, retirement, and peer-runnable invalidation | same scheduler/thread owners plus signal boundary | missing as one lock-free continuation contract |
| CPU timer armed/deadline/generation publication | `src/runtime/hl-runtime/src/process/itimer.rs::AlarmRegistry` | missing; CPU timers remain in `Mutex<BTreeMap<(ProcessId, i32), TimerState>>` |
| Per-quantum CPU accounting usable without task-registry locking | `src/runtime/hl-task/src/registry/state.rs::TaskRegistry::charge_cpu` and scheduler `charge_elapsed` | missing |
| Optional borrowed native poll ABI and cumulative grants | `src/runtime/native/exec/include/executor.h`, `src/runtime/native/exec/src/arch/x86_64/run.c` | missing |
| Existing bounded REP correctness across widths, DF, overlap, dirty and epoch exits | `run.c::{rep_decode,rep_span,rep_copy,rep_fill,rep_execute}` | implemented; must be preserved |

### CPU-accounting continuation audit

The retained implementation inspected was
`../engine/src/linux_abi/syscall/rare.c::syscall_rare` cases 102/103 and
`../engine/src/linux_abi/host_signal.h::{setitimer,getitimer}`. On POSIX it
delegates all three interval timers to the host process, whose kernel owns
timer identity, locking, CPU charging, signal generation, and teardown; the
Windows seam explicitly returns `ENOSYS`. There is no retained per-activation
account object or architecture branch beyond that host split.

The Rust path inspected was
`hl-task::registry::{Process,TaskRegistry::charge_cpu,cpu_usage,make_zombie}`,
checkpoint restore/snapshot and child reaping, plus
`hl-engine::execution::{ThreadRun,ThreadSet::stage_inner,prepare_image}` and
`scheduler::{run,advance,charge_elapsed}`. The process-lifetime `CpuAccount`
now supplies atomic saturating publication. Every admitted `ThreadRun` retains
its account, exec-image preparation clones it, fork staging obtains the new
process account, checkpoint restores its value, and snapshot/wait/reap observe
it. PID-slot reuse installs a distinct `Arc`; therefore a late scheduler charge
can affect only the retired generation. Quantum charging no longer acquires the
task-registry mutex. Signal/cancellation boundaries still force `charge_elapsed`
on return from `advance`; CPU-timer polling remains at the existing signal
boundary. Lock-free CPU-timer deadline/generation publication itself remains
missing in `hl-runtime::process::itimer::AlarmRegistry`.

The largest locally bounded prerequisite is the mapping-request generation,
but it is not a one-file change: every map, unmap, protect, batch, remap,
external-write, executable-publication, and checkpoint transaction ingress
must invalidate before lock acquisition, while read-only projections capture
the epoch after admission. Landing only selected mutation paths would make the
token unsound. The scheduler and CPU-timer prerequisites cross additional
owners. Therefore this lane does not add a callback or silently refill
`request->budget`; either would admit work while a missing invalidator waits.

### Quiet-baseline attempt

An exact-tree release build completed with all 18 logical CPUs in 1 minute 52
seconds (`CARGO_BUILD_JOBS=18 cargo build --release -p hl-engine -p testing`).
The direct benchmark attempt was pinned to CPU 17 and supplied engine options
only through `HL_COMPAT_ENGINE_OPTIONS='HL_NATIVE_EXECUTION=1'`. It could not be
accepted as diagnostics-off evidence: the benchmark adapter re-enabled native
diagnostics, then the full checked-in AMD64 combined workload ended in a typed
fault after 7,895 public runs (609 builds, 3,054,416 hits, 81 fallbacks) instead
of the expected exit zero. The matrix runner was also unavailable for AMD64 on
this ARM64 host because it requires a configured host-native baseline. No
timing from either rejected attempt is reported as a baseline. The last audited
quiet diagnostics-off comparison remains the exact-clean `7ac681960` evidence
in `/Users/x/dd/.performance/verify_x86_7ac/RESULTS.md`; it is useful historical
evidence, not evidence for this commit.

## Acceptance tests

Implementation requires fail-first tests for the complete mechanism:

- greater-than-one-MiB REP continuation with the exact poll count, cumulative
  per-element accounting, RIP, RCX, RSI, and RDI at every denial and resume;
- all MOVS/STOS widths, forward and backward DF, forward-overlap smear, and
  disjoint ranges;
- a source fault and destination fault in the next view after prior quanta,
  proving completed bytes remain committed and the faulting element does not;
- exact dirty ranges, dirty-capacity fallback, executable-alias epoch exit,
  and no instruction replay;
- interrupt and deliverable signal at every poll position with an exact signal
  frame PC and register image;
- a concurrent checkpoint request completing within one quantum, with the
  completed writes in its snapshot and no admission deadlock;
- a queued mapping transition denying continuation within one quantum and
  proceeding after projection release;
- a parked peer becoming runnable and receiving control within one quantum,
  while a run that begins shared receives no refill;
- lock-free per-quantum CPU accumulation and exact CPU-timer expiry when that
  policy is selected, including denial before lock-taking signal publication;
- fail-closed denial for every armed timer or accounting-sensitive process
  when the conservative policy is selected;
- timer arming immediately before, during, and after a poll, proving the token
  is invalidated before the state becomes visible; timer disarming must not
  permit a stale activation to refill, while a fresh generation may do so;
- process control, retirement, cancellation, and generation replacement;
- zero budget, zero REP count, arithmetic overflow, poll denial, poll error,
  and caught callback panic;
- differential results against the retained engine for partial faults, DF,
  overlap, register publication, and final bytes.

After warning-strict native, Rust, scheduler, memory, checkpoint, and
differential tests pass, an exact-clean five-repeat benchmark must preserve the
checksum and `native-verified` proof and report public exits, median, and p90
against both the 119,295-us accepted Rust row and the 6,531-us retained-C row.
Until that measurement exists, public-exit reduction remains the only expected
effect and no end-to-end performance improvement is claimed.

## REP bulk lifecycle and capability audit

This audit records the retained-C oracle studied before changing the Rust/native
REP path. The retained tree is read-only at `/Users/x/dd/engine`.

### Retained implementation studied

- `src/translator/guest/x86_64/rep_runtime.c`: `hl_x86_rep_movs`,
  `hl_x86_rep_stos`, the scalar and pinned MOVS/STOS loops,
  `rep_element_read`, `rep_element_write`, and `rep_fault`.
- `src/translator/guest/x86_64/lower/repstr.c`:
  `hl_x86_lower_repstr` and `emit_rep_string`.
- `src/translator/guest/x86_64/rep.c`: `hl_x86_rep_compare`.
- `src/translator/guest/x86_64/interp.c`: string instruction cases and
  `run_block` dispatch.
- `src/translator/guest_memory.c`: indirect read, write, pin, and unpin seams.
- `src/core/target/x86_64.c`: guest-memory adapters, validator binding,
  soft-TLB misses, and store observation.
- `src/linux_abi/logical_vma.c`: `hl_logical_vma_pin_data` and
  `hl_logical_vma_unpin`.
- `src/translator/guest/x86_64/emit.c`: deferred executable-store observation
  and drain.

### Oracle contract

The AArch64-host translator lowers REP MOVS and STOS, opcodes A4/A5 and AA/AB,
at widths 1, 2, 4, and 8 into bulk helpers when there is no segment override or
32-bit address size. Non-REP forms and LODS remain scalar. CMPS and SCAS have a
separate flag- and early-stop-aware helper and are not blind bulk copies.
Non-AArch64 hosts execute MOVS/STOS/LODS elementwise.

The emitted helper spills architectural state and passes original RDI, RSI or
RAX, RCX, direction, and the faulting RIP. It returns completed elements. The
epilogue advances RDI and RSI by signed `completed * width`, subtracts completed
from RCX, and leaves flags unchanged. A partial fault records the current guest
address, width, access, REP RIP, and soft-miss reason before those exact register
updates. Retry therefore resumes the same instruction with residual RCX. MOVS
orders each element's source read before destination write. Zero count completes
without touching memory.

Forward indirect MOVS pins source READ before destination WRITE, copies only the
minimum contiguous whole-element span, then unpins destination and source.
Forward STOS similarly pins a write span. Pin lookup holds the VMA mutex only
while validating and acquiring a backing reference; copy/fill runs unlocked;
unpin reacquires the mutex and releases the reference. Every error path unpins.
Pins stop at a VMA boundary and the outer loop reacquires the next span. Unsafe
overlap and backward indirect operations use exact scalar element order.

The direct bulk path validates the complete source range before the destination
range. Guest-memory indirection performs permission checks in the resolver;
otherwise the direct validator runs before raw access. A malformed or denied
pin faults at the current element. Forward overlap deliberately smears at the
architectural element width rather than using `memmove`; backward operations
copy or fill highest-to-lowest. STOS writes the low width bytes of RAX.
Non-PIE rebasing affects host dereferences only, never guest register values.

Successful scalar stores are observed per element. Bulk stores publish one
range only after the memory operation and after pins are released. Executable
alias work is drained only after RDI, RSI, and RCX describe completed progress,
so an SMC exit cannot replay or skip stores.

The retained helper has no instruction-budget accounting and does not poll
signals or cancellation inside a large operation. Signals are handled at block
or dispatcher boundaries. Rust intentionally improves bounded responsiveness:
it caps chunks, charges completed elements to the budget, and observes interrupt
state before each chunk while preserving the same partial-progress contract.

### Rust ownership and capability map

`src/runtime/native/exec/src/arch/x86_64/run.c` owns the bounded MOVS/STOS fast path.
`rep_decode` admits the relevant REP widths, `rep_span` bounds work to the current
view and one MiB, `rep_copy` and `rep_fill` preserve overlap and direction, and
`hl_x86_projection_resolve` plus `hl_x86_projection_written` own permission and
dirty publication. The synchronous Rust `ProjectionLease` holds authenticated
mapping state stable for the run; this replaces retained repeated VMA pinning.
No allocation or lock acquisition occurs in the native bulk loop.

The four-entry `x86_run_views` table is a generated-code locality cache, not a
mapping-capability boundary. The request projection is already validated,
bounded to `HL_X86_PROJECTION_MAX_VIEWS`, incarnation-checked, and held by the
lease. Bulk lookup must therefore search the cache first and then the complete
projection. Resolver-acquired dynamic views remain reachable through the cache.
Missing or denied views still fail closed to the scalar path, which owns precise
resolver and fault exits.

MOVS/STOS widths 1/2/4/8, zero count, both direction values, overlap semantics,
view splitting, exact partial progress, budget charging, permission ordering,
and post-write dirty publication are implemented. LODS remains scalar. CMPS and
SCAS remain with their distinct compare/scan semantics and must not be folded
into this copy/fill mechanism.

### Performance attribution

The checksum benchmark's measured phase performs 90 one-MiB copies. With a
65,536-element run budget, each copy requires 16 native runs: 1,440 REP quanta.
The measured Rust proof reported 1,489 native runs, so budget boundaries explain
96.7% of run entries. Against the retained C result, the remaining gap is about
15.82 microseconds per quantum. This projection-cache correctness change does
not remove that scheduler/run-boundary cost; changing the architectural budget
or public-exit contract is a separate lane requiring explicit evidence.


## Writable-view fallback and REP reservation evidence

### Scope and retained oracle

This report profiles the alternating writable-view path at `ca6b873ac`, which
contains `cc4845ca8` (`native: cache x86 writable projections`). The retained
C oracle was inspected read-only at
`/Users/x/dd/engine` (`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`). The complete
corresponding domain and entry points are recorded in
`WRITE_PUBLICATION.md`; this follow-up mechanically checked the hot-path
ownership and fallback behavior against:

- `src/translator/guest/x86_64/emit.c`: `emit_memory_guard`,
  `emit_soft_guard`, `emit_soft_store_observe`, and
  `emit_soft_store_commit`;
- `src/translator/guest/x86_64/translate.c`: `rm_load_access`, `rm_store`,
  `rm_store_after_guard`, and the scalar, SIMD, x87, atomic, and string
  callers;
- `src/translator/guest/x86_64/rep_runtime.c`: scalar and pinned `MOVS` and
  `STOS`, including partial completion;
- `src/translator/guest_memory.c`: data resolution and pin lifetime;
- `src/linux_abi/logical_vma.c`: `hl_logical_vma_resolve`,
  `hl_logical_vma_resolve_data`, `hl_logical_vma_pin_data`, and
  `hl_logical_vma_unpin`; and
- `src/core/target/x86_64.c`: direct access admission and executable-alias
  observation.

The retained engine's immutable logical-VMA snapshot provides lock-free
binary-search resolution. A pin takes a backing reference under the ledger
mutex and releases the mutex before accessing bytes. The ordinary direct
mapping is an identity access; an indirect mapping resolves or pins before
mutation. A store is observed only after success, and REP preserves partial
progress. Mapping publication owns generation change and retired-snapshot
lifetime. The AArch64-host x86 lowering is the optimized implementation; other
hosts use the interpreter path. There is no fixed per-store dirty journal in
the retained hot path.

Husklet's corresponding owners are the Rust `ProjectionLease`, x86
`view_publish`, `frontend/memory.c::emit_write_cache`, the scalar/vector/RMW
emitters, `projection.c::flush_dirty`, and `NativeX86::writes`. Mapping
identity, backing lifetime, permission admission, pre-mutation capacity
failure, post-success publication, executable invalidation, and REP partial
completion are implemented. The material divergence is the bounded exact
dirty journal described below.

### Finding

The cached writable-view selector removes resolver callbacks but does not
remove dispatcher exits for a sustained alternation. Each transition away
from an active dirty owner appends four words to `dirty_records`:

```text
view_first, view_last, dirty_first, dirty_last
```

`HL_X86_DIRTY_CAPACITY` is 16. `emit_write_cache` tests `dirty_count` before
changing the active owner and deliberately falls through to the ordinary miss
path when it is full. The dispatcher can resolve the already-cached view, but
`projection.c::flush_dirty` then sets `dirty_overflow`; the run must return to
Rust so `NativeX86::writes` can conservatively publish and begin a fresh lease.
Thus a two-view loop can execute entirely from published views while still
crossing translated code, C dispatcher, Rust lease publication, and re-entry
roughly once per 16 owner transitions.

This directly explains a count on the order of 60 million fallback boundaries:
the count is journal saturation, not 60 million missing projections. The
diagnostic `boundary_fallback` counts both internally serviced projection
misses and final unsupported-instruction exits. `operand_callbacks` and
`operand_cache_hits` must be reported alongside it; the current two-view path
should have no callback after both views are published.

The focused regression in `test/x86_continue.c`,
`alternating_writable_views_stay_native`, runs only two loop iterations. It
expects three archived records, including a duplicate record for the first
view and exact same range. It therefore proves correct owner attribution but
cannot reach the 16-record saturation threshold. `test/x86_projection.c`
explicitly alternates until it expects `dirty_count == 16` and
`dirty_overflow == 1`, cementing the current expensive behavior rather than
testing sustained native progress.

### Required generic correction

Do not merely enlarge the array: it divides the exit count by a constant and
increases every CPU state. The coherent mechanism is an exact interval journal
that coalesces a completed interval with an existing record only when both
have the same projection owner and overlap or are adjacent. This preserves the
`publish_written_ranges` contract: every byte in the union is proven written,
while repeated writes to the same address consume no new capacity. Disjoint
ranges remain separate, and a genuinely full journal still fails before the
guest mutation.

The correction must be shared by emitted `emit_write_cache` archival,
`projection.c::flush_dirty`, and the REP capacity preflight. Tests must cover:

1. more than 16 repeated alternations between two exact ranges without an
   internal fallback or overflow;
2. overlapping and adjacent same-owner intervals coalescing exactly;
3. disjoint same-owner intervals remaining distinct;
4. identical guest ranges under distinct projection owners remaining
   distinct; and
5. 17 genuinely disjoint intervals preserving pre-mutation capacity failure.

The follow-up implementation applies identical overlap/adjacency coalescing to
the emitted scalar/vector/RMW transition and dispatcher
`projection.c::flush_dirty`. The sustained translated test now performs 64
two-view iterations (128 stores and 127 owner transitions), remains native,
and retains two archived exact records without overflow. The projection test
also preserves a full-journal case whose different owner cannot coalesce and
therefore still fails closed.

The REP bulk preflight remains a separate gap. `rep_dirty_full` sees only the
full count and prospective owner, not whether the current interval can merge
with a record. It can therefore request an epoch earlier than necessary. That
path was not weakened here: REP performs large bulk operations, so its exit
rate is not the scalar alternating-store amplification measured by this lane.
Giving it identical coalescing requires factoring one bounded journal
reservation operation shared by preflight and post-success publication; doing
only a post-write merge would violate the pre-mutation capacity contract.

### Exact integrated-tree measurement

Root integrated this stack as `a8a49f1cb2fb6e279a68b01ccf7d5896885ac185`.
The release engine and runner were built from that clean detached tree into
`/Users/x/dd/husklet-targets/x86-coalesce-a8a49f1`, outside `/tmp`. A single
CPU-17 diagnostic proof reported `native-verified`, checksum `7190`, 1,489
runs, 118 builds, 66,810 hits, four final fallbacks, 73,604 completed native
instructions, 1,236 operand callbacks, and 29 operand-cache hits.

The diagnostics-off comparison then used the same clean-repro x86 guest,
retained C runner, divisor 100, and memory phase as the `ca6b873ac` baseline.
Seven cycles rotated QEMU, C, and Rust order on exclusive CPU 17. Every one of
the 21 rows returned checksum `7190`:

| provider | samples (microseconds, sorted) | median |
|---|---|---:|
| retained C | 1580, 1700, 1811, 1831, 1874, 1886, 1902 | 1831 |
| Rust native | 23833, 24033, 24084, 24610, 25374, 25534, 25644 | 24610 |
| QEMU | 5960, 6451, 6512, 6609, 6637, 6704, 7160 | 6609 |

Rust is 13.441 times retained C in this run, down from the exact `ca6b873ac`
ratio of 15.430 times: the relative gap ratio fell 12.89%. The Rust median
itself fell from 25,892 to 24,610 microseconds, a 4.95% improvement. This
memory phase is REP-heavy, so the measurable improvement is intentionally
bounded by the still-conservative REP preflight described above.

Content identities were:

- Rust engine: `44b2bb9f7b537bd16753a53d2189b3e4f536da776d4436d7dc7f592eb4b4e045`;
- testing runner: `9e770dfff59994088dc3b4161cbb3b2dd0fc677c4bb85913b4897c6aa1c9bde8`;
- guest: `bda1b267655938e7be77cd2ec0450c7095650437e4a5e7be10db81da3a973b9d`;
  and
- retained C runner: `0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62`.

Raw evidence is retained in the durable target's `evidence/diagnostics.csv`
and `evidence/timing-direct-quiet-3/` directory.

### REP reservation completion

The retained REP path was rechecked before changing Husklet's preflight:
`rep_runtime.c` pins the largest proven source and destination spans, caps work
at both boundaries, updates RCX/RSI/RDI only for completed elements, and
returns the first uncompleted element to the dispatcher. A permission failure
before the first element changes neither bytes nor registers. The existing
Husklet boundary test has the same partial-fault contract: it completes four
bytes, advances RCX/RSI/RDI by four, then falls back at the first read-only
destination byte.

`hl_x86_projection_switch_writable` now performs the shared pre-mutation
capacity decision used by REP. An unchanged active owner, an empty current
interval, free capacity, or a same-owner overlapping/adjacent archived record
authorizes the switch. Otherwise it refuses before `rep_copy` or `rep_fill`.
The already-shared dispatcher flush performs the matching post-success merge;
scalar/vector/RMW emitted code implements the identical bounded scan without
a per-store C call. A new full-journal REP test proves that a mergeable prior
range permits eight elements and preserves the 16-record bound. Its sibling
nonmergeable full-journal test still exits epoch with registers and bytes
unchanged. Existing budget-limited and permission-boundary cases preserve
partial progress and the first-uncompleted-element contract.

## Residual-exit and continuation evidence

The retained implementation was inspected read-only in
`../engine/src/core/dispatch.c` (`run_block`, `block_return`, `run_guest`),
`../engine/src/translator/guest/x86_64/emit.c` (`emit_prologue`,
`emit_spill_gpr`, `emit_spill`), and
`../engine/src/translator/guest/x86_64/translate.c` (block entry interrupt and
budget checks, conditional backedges, and typed returns). The dispatcher owns
CPU and cache lifetime. A translated block retains guest GPR and XMM state in
host registers, observes interrupts at bounded entry points, and returns
completed work before the dispatcher services a boundary. These operations
allocate no memory, acquire no lock, invoke no host service, and have no errno
or cancellation behavior. Cache generation and source identity determine
whether a continuation remains executable; invalidation restores a typed
dispatcher edge before retirement.

The current owners are `src/arch/x86_64/run.c` (`emit_block` and
`hl_native_x86_64_run`), `src/arch/x86_64/frontend/output.c`
(`hl_x86_checkpoint`, `hl_x86_finish_chain`, `hl_x86_emit_exit`), and the
generic cache relocation layer. Their capability comparison is:

| Capability | Retained C | Native implementation |
|---|---|---|
| Entry budget and interrupt check | Before translated entry | Implemented |
| Exact instruction charging | At dispatcher return | Implemented |
| Backedge batching | Internal, not guest-visible | Implemented in 256-iteration quanta |
| Interrupt and executable-write visibility | Bounded translated boundary | Implemented at every quantum |
| Continuation identity | Live translation generation | Mapping epoch, instruction epoch, identity token, and source interval |
| Invalidation | Retire matching translation and links | Implemented by generic cache relocation |
| Public yield | Only when the caller's budget is exhausted | Divergent: leaked every internal 256-iteration quantum |

The divergence made an internal polling quantum observable as a public yield.
For a three-instruction backward loop with a 1,000-instruction budget, the
first quantum returned with 232 instructions still available, register state
at iteration 256, and `executed=768`. The run loop now continues internally
after a valid quantum. Its next ordinary loop iteration checks the remaining
budget and interrupt before re-entry, so the 256-iteration bound is unchanged;
only the premature public boundary is removed.

`test/x86_continue.c` covers register and projected-memory vector operations,
finite fallthrough, exact counter and completion accounting, exact budget
exhaustion at one quantum, interrupt-before-entry, and translation
invalidation. The fail-first state was `kind=YIELD`, `pc=0x8000`, `rax=44`,
`executed=768`, `budget=232` for the finite 300-iteration control. After the
change, warning-strict exact-source builds of `x86_continue`, `x86_translation`,
and `x86_budget` pass.

### Request budget smaller than a block

The retained comparison was extended across the complete execution boundary:

- `../engine/src/core/dispatch.c`: `run_guest`, including cache ownership,
  translation publication, `run_block`, reason handling, signal polling, and
  teardown from the thread and stop-the-world registries;
- `../engine/src/translator/guest/x86_64/translate.c`: `translate_block`,
  `run_block`, and `block_return`;
- `../engine/src/translator/guest/x86_64/emit.c`: the typed exit and complete
  register-spill emitters;
- `../engine/src/translator/guest/x86_64/dispatch.h`: x86 dispatcher entry,
  reason, cache, and interrupt hooks; and
- `../engine/src/translator/guest/x86_64/cpu.h`: dispatcher-owned CPU state and
  baked assembly offsets.

The retained engine has no request-sized instruction slice: it enters a whole
decoded block, whose terminal writes the next architectural RIP and reason,
then `block_return` restores the host callee-saved state. The dispatcher owns
the CPU for the guest thread's lifetime, holds the cache lock only for lookup
or publication (never across execution or host service), polls signals at
spilled block boundaries, and unregisters the CPU only after guest exit. There
is no partial-result, errno, blocking, or cancellation path inside a translated
block. AArch64 host assembly is the only working x86 translation backend; other
hosts abort rather than enter AArch64 code.

The Rust native backend adds a caller-owned instruction budget. A block is an
atomic admission unit: when `budget < instruction_count`,
`hl_native_x86_64_run` returns `YIELD` with unchanged RIP and budget and
`executed == 0`. Retrying that same request cannot make progress. The engine
scheduler now interprets one instruction at this precise non-progress boundary
and records the site through the existing generic fallback mechanism. A yield
after any completed instruction remains an ordinary scheduler yield. This
preserves the native block's atomic state contract while guaranteeing that a
supported instruction stream either consumes budget natively or advances via
the interpreter; it does not manufacture partial native execution.

### Projected dynamic-return progress

The AMD64 SysV shared-memory-lock investigation stopped inside
`hl_native_x86_64_run` while the architectural PC named a two-instruction tail,
`mov [r9],rdx; ret`. A stale branch reason or a zero-accounting dynamic-chain
cycle was considered, but the instruction shape alone does not reproduce that
failure. `projected_return_progress` executes the exact bytes with an authorized
write destination and a 64-entry authorized return stack whose entries all
target the same tail. This warms and then repeatedly uses the indirect-branch
cache. A 64-instruction budget returns a typed yield with exactly 64 completed
instructions, zero budget, 32 consumed stack entries, the original PC, and the
expected committed destination value.

The warning-strict direct native build and a five-second bounded execution pass.
This proves the exact projected write/return cycle charges progress and reaches
its public budget boundary. It also proves that converting a branch at this
site to interpreter fallback would risk replaying an already committed write.
No production progress guard is justified by this trace alone; the broader
runtime hang requires a trace that distinguishes time spent inside generated
code from time spent in the C dispatch loop before either owner is changed.
