# AMD64 REP quantum audit

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
- `src/native/exec/src/arch/x86_64/run.c`:
  `rep_decode`, `rep_view`, `rep_span`, `rep_execute`,
  `hl_native_x86_64_run`, and `leave_exit`.
- `src/native/exec/include/executor.h::hl_native_run_request`, whose `budget`
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
| Optional borrowed native poll ABI and cumulative grants | `src/native/exec/include/executor.h`, `src/native/exec/src/arch/x86_64/run.c` | missing |
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
