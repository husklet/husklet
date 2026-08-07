# Write publication audit

This audit records why per-view coarse write publication is a promising native
execution optimization, but is not yet safe to enable. It was made against
Husklet `7e368ec300ad6c3a136ce1b0b2ec052a34fc6306` and the retained C engine in
`/Users/x/dd/engine` on 2026-08-04.

## Retained C oracle

The retained engine was inspected at these ownership boundaries:

- `src/core/dispatch.c`: `run_guest` owns dispatcher entry/return, the JIT lock,
  mapping transitions, cache rotation, and stop-the-world coordination.
- `src/translator/guest/aarch64/translate.c`: guest loads and stores are emitted
  against directly mapped guest memory; it does not maintain a per-store host
  publication journal.
- `src/translator/guest/aarch64/stubs.c`: block entry/return spills architectural
  state and the self-modifying-code exit records the invalidated guest address.
- `src/translator/guest/aarch64/dispatch.h`: `smc_icflush` and `smc_commit` own
  executable translation invalidation. Dirty executable ranges are queued until
  the architecturally visible synchronization point; overflow conservatively
  refreshes the complete range.
- `src/translator/guest/aarch64/cpu.h`: the CPU owns bounded SMC ranges and their
  overflow state. No ordinary writable-mapping dirty interval is recorded.
- `src/translator/guest/aarch64/signal.c`: signal entry reconstructs guest state
  from the native fault context before returning to the dispatcher.

The retained engine relies on direct host mappings for ordinary stores and on
the operating system for shared-map coherence. Precise bookkeeping exists for
executable invalidation, not for every ordinary store. Its state is CPU-local
while executing and is consumed under dispatcher/JIT synchronization. Mapping
teardown and cache rotation prevent translations from surviving the mapping
identity they were compiled against.

## Rust capability comparison

| Capability | Retained C | Current Rust |
| --- | --- | --- |
| Ordinary private write | Direct store | Per-store interval reservation and merge |
| Coherent shared write | Direct store | Per-store interval reservation and merge |
| Non-coherent shared write | Not an equivalent retained path | Exact range required for reconciliation |
| Executable write | Precise SMC range and synchronization | Exact dirty range and invalidation token |
| Mapping identity | Mapping/JIT generation | `mapping_incarnation` on every projected view |
| Partial/faulting store | Fault context decides completion | Pre-store journal capacity check, post-store dirty commit |
| Journal overflow | Conservative SMC refresh | Epoch exit before an unrecordable store |

In Rust, `hl_a64_guard_write_begin` and `hl_a64_guard_written` in
`src/native/exec/src/arch/aarch64/guard.c` run around every emitted store.
`projection.c::flush_dirty` moves the active interval into the bounded journal.
The x86-64 projection path has the same publication obligation. Then
`ProjectionLease::publish_written_ranges` in
`src/runtime/hl-memory/src/mapping/projection.rs` reconciles shared backing,
invalidates exclusives, commits the reservation, increments the epoch, and
publishes executable invalidations.

The native ABI cannot currently carry a publication policy. The reserved field
of `hl_native_projection_view` is required to be zero by both validators, and
`executor.c::run_view_publish` copies only address bounds, delta, and permissions
into the CPU view cache. Reinterpreting `reserved` would therefore create an
unchecked ABI convention and still would not qualify the active policy by the
published mapping incarnation.

## Safe optimization boundary

### 2026-08-05 owner-metadata prerequisite audit

The append-only owner-metadata slice rechecked retained C
`src/core/dispatch.c::run_guest`, AArch64
`translate.c::smc_commit`/`smc_icflush`, `stubs.c` block entry and return, and
`dispatch.h` reason handling. Retained mappings own identity and lifetime under
the dispatcher/JIT and stop-the-world locks; ordinary private writes remain
direct host stores, while executable writes retain CPU-local precise ranges
until dispatcher synchronization. Mapping replacement and teardown invalidate
translated identity before reuse. There is no retained per-application policy,
per-store callback, or publication index to copy.

The Rust prerequisite therefore changes no publication behavior. Every live
projection receives `Exact`; primary index zero and additional insertion
ordinals remain stable when the bounded native cache promotes entries. The
existing release-token publication makes the old four-word view and appended
policy/index pair visible as one generation-qualified record. Resolver faults,
generation changes, run reset, and callback transitions clear stale active
metadata before another owner is installed. Coarse publication remains absent.

A view may use coarse publication only when it is non-executable and either
private or backed by a host-coherent shared mapping. Non-coherent shared views
remain exact because untouched bytes must not be reconciled from stale host
storage. Executable views remain exact because invalidation ordering and tokens
are part of the execution contract. Primary and additional views must retain
independent policies.

The coherent implementation must be one change spanning these boundaries:

1. Add an explicit `WritePublication::{Coarse, Exact}` to each memory projection,
   derived from backing coherence and execute protection.
2. Extend the versioned native ABI and generated CPU layout explicitly. Publish
   policy together with address bounds and `mapping_incarnation`; never infer it
   from a stale active view.
3. After a successful coarse store, mark that generation-qualified view dirty
   without interval merging. Never mark before a potentially faulting store.
4. Return coarse dirty view identities alongside exact dirty ranges. Mixed views
   in one run are required. Completed writes must survive interrupt and epoch
   exits; failed stores must publish nothing.
5. In the lease, publish a coarse view as its complete range while retaining
   exact reconciliation and executable invalidation for exact views. Commit or
   roll back every reservation exactly once, including partial-error paths.

Required focused evidence is: policy classification for private/coherent shared,
non-coherent shared, and executable views; mixed primary/additional views;
generation churn; a faulting store; interrupt and epoch exit after a completed
store; exact shared reconciliation; disjoint executable invalidations; and AArch64
and x86-64 parity. Only after those pass should the memory benchmark compare the
exact commit against a pinned retained-C revision. A test-only draft under
`/tmp/husklet-pubpolicy.0xbFWH/tree` usefully states the classification cases but
does not implement or prove the ABI, fault, epoch, or mixed-view invariants.

## Blocker

Changing only memory-side classification does not remove the hot per-store work.
Changing only the store guards risks losing completed writes or publishing a
write that faulted. The smallest safe optimization therefore crosses the memory
lease, both native ABIs, generated CPU layouts, emitted AArch64/x86-64 guards,
and executor result decoding. No partial performance patch is justified until
that generation-qualified contract is implemented and tested as one coherent
slice.

## Executable-alias prerequisite audit

The follow-up audit at Husklet `5e58a7104a3754d1b0fcc020f42f37591b11f1d3`
studied the retained mapping and SMC ownership in:

- `/Users/x/dd/engine/src/core/dispatch.c::run_guest`, which holds mapping and
  JIT transition synchronization at dispatcher boundaries;
- `/Users/x/dd/engine/src/translator/guest/aarch64/translate.c::smc_icflush`
  and `smc_commit`, which retain precise executable-source invalidation until
  the guest synchronization point;
- `/Users/x/dd/engine/src/translator/guest/aarch64/dispatch.h`, whose
  `R_ICFLUSH` and `R_ICCOMMIT` paths consume that state;
- `/Users/x/dd/engine/src/translator/cache.c::jit_after_fork`, which resets or
  generation-separates inherited translation ownership after fork.

The Rust comparison covered `ledger.rs` region splitting and protection,
`mapping/host.rs::{protect,fork_restore}`, `mapping/remap.rs::remap_with_charge`,
checkpoint snapshot/restore, and the existing shared-backing alias walk in
`Coordinator::executable_write_ranges`. Mapping mutations are serialized by the
coordinator transaction and checkpoint admission; `ProjectionLease` retains
both for its lifetime. The ledger can therefore issue evidence containing its
current generation and whether any live region with the selected backing
identity is executable. Split regions, protection changes, remaps, and restored
fork/checkpoint ledgers are covered because they publish complete region sets
and advance or preserve the ledger generation as appropriate.

This evidence is deliberately conservative: any executable region sharing the
identity rejects coarse publication even when backing offsets do not overlap.
It covers one address-space ledger. It is not proof against aliases in another
coordinator, so shared backing remains ineligible until the shared-object owner
provides equivalent global evidence. A future publication classifier must read
the evidence while already holding the projection lease transaction, retain the
recorded generation, and fall back to exact publication if either condition is
not satisfied.

## x86 writable-view cache contract

The 2026-08-05 audit inspected retained C
`translator/guest/x86_64/{emit.c,translate.c,rep_runtime.c}`,
`translator/guest_memory.c`, `linux_abi/logical_vma.c`, and
`core/target/x86_64.c`. The retained logical-VMA table owns mapping identity and
backing lifetime under its mutex; pins retain backing while host bytes are used,
without holding the lock across copying. Permission and span checks precede a
store, successful stores are observed afterward, faults report the first
incomplete address, and REP preserves partial progress. Resolver errors cross
the dispatcher boundary; emitted code owns no cancellation or errno conversion.

| Capability | Rust owner | Status |
| --- | --- | --- |
| Mapping lifetime and generation | `ProjectionLease` and x86 `view_publish` | Implemented |
| Bounded direct view lookup | `read_views` publication in `run.c` | Implemented for reads and writes |
| Permission/span proof before mutation | `frontend/memory.c` guards | Implemented |
| Exact dirty owner/range journal | x86 CPU dirty fields and records | Implemented |
| Archive before projection-owner change | x86 writable-view cache | Implemented |
| Capacity rejection before mutation | x86 writable-view cache and dispatcher | Implemented |
| Successful-store publication | scalar, vector, and RMW emitters | Implemented |
| Executable-write sticky latch | scalar, vector, and RMW emitters | Implemented |
| REP partial completion | `run.c::rep_execute` | Implemented separately |

Writes now consume the four run-authenticated views without a callback. Before
changing the active dirty owner, the emitter archives its exact record; a full
journal exits before mutation. Dirty bounds and executable-write state publish
only after a successful host mutation. This covers scalar widths 1/2/4/8,
write-qualified arithmetic loads, XCHG, XADD, CMPXCHG, and vector stores. REP
retains its separately preflighted projection and exact post-success publication.

## Executable-write precision contract

The 2026-08-05 executable-alias audit additionally inspected retained C
`core/target/x86_64.c::{jit86_store_alias_range,jit86_store_alias_changed,jit86_smc_commit}`,
`translator/guest/x86_64/translate.c::jit86_drop_range_translations`,
`translator/cache.c::map_invalidate_source_ranges`, and the `G_SMC_UNMAP` and
`G_SMC_COPYOUT` paths in `translator/guest/x86_64/abi.h`. The retained engine
separates writable bytes, executable aliases, mapping identity, and cache
lifetime. Its file-map lock protects backing identity, CPU-local store and SMC
ranges drive writeback and invalidation, and overflow may retire all translations
but never expands writeback to a whole view. Commit occurs within mapping
stop-the-world coordination and clears both journals before resume.

The x86 native resolver must keep each cached window permission-homogeneous.
Host-contiguous guest views with different complete permissions cannot coalesce:
otherwise a store through an adjacent RW view can inherit an executable bit and
force a spurious epoch exit. Cross-boundary operations use the existing operand
resolver/interpreter path, admitting no partial native store.

| Capability | Retained C | Rust contract |
| --- | --- | --- |
| Actual written bytes | per-CPU `store_ranges` | bounded native dirty journal |
| Executable alias discovery | logical VMA/file-backing overlap | `executable_write_ranges` backing overlap |
| Translation retirement | source-range invalidation | executable page tokens/ranges |
| Permission transitions | mapping STW plus range drop | ledger generation plus projection exclusion |
| Shared writeback | store journal, never SMC overflow | reservation commit plus exact reconciliation |
| Fork/checkpoint | cache-generation repair and STW image | fresh coordinator/projection lifetime |
| Mixed-permission host-contiguous views | distinct logical VMAs | distinct native cached windows |

Exact dirty publication, shared-alias projection, mprotect generation changes,
fault ordering, fork, and checkpoint ownership remain unchanged by this rule.

## Projection authority and executable identity audit (2026-08-05)

This focused slice studied the retained C authority and SMC lifecycle in
`/Users/x/dd/engine/src/translator/guest/aarch64/translate.c` at
`aarch64_soft_tlb_miss`, `aarch64_soft_tlb_span`,
`aarch64_soft_prepare_bounce`, `aarch64_soft_bounce_commit`, and
`aarch64_smc_copyout`; `src/translator/guest/aarch64/cpu.h` at the soft-view
and SMC fields; `src/translator/guest/aarch64/dispatch.h` at the soft-miss,
span, and commit returns; `/Users/x/dd/engine/src/core/target/x86_64.c` at
`soft_tlb_miss`, `jit86_store_alias_changed`, and `jit86_smc_commit`; and
`src/translator/guest/x86_64/{emit.c,rep_runtime.c,dispatch.h}` at store
permission checks, completed-store observation, partial REP progress, and SMC
return handling.

In the retained engine, a logical-VMA snapshot owns the complete mapped
protection and backing identity. A CPU owns its cached view and bounded dirty
and SMC ranges until dispatcher return. Snapshot publication supplies the
lifetime of the cached host delta; mapping and JIT stop-the-world transitions
exclude replacement and teardown while ranges are consumed. A miss checks the
operation's requested READ or WRITE bits before mutation, while the complete
mapped EXEC bit remains available to classify a successful write as
self-modifying code. Cross-span stores validate all pieces before scatter;
failed resolution faults without publication, completed writes survive the
dispatcher exit, and x86 REP retains partial progress. AArch64 additionally
blocks host signals around the validated bounce/scatter interval. Host-specific
direct-map fallback is present on non-Apple AArch64 and the x86 span path has an
Apple-specific adjacent-view check, but neither branch grants data access from
the mapped permissions after the requested access check fails.

The corresponding Rust owners are
`hl_memory::MappingCoordinator::project_contiguous` for admission, mapping
transaction exclusion, backing validation, requested authority, complete
mapped protection, generation, write reservation, and lease teardown;
`ProjectionLease::{allows,into_direct,publish_written_ranges}` for invocation
authority and exactly-once publication; and the AArch64 and x86 lease runners
in `hl-engine` for native view publication and dispatcher results. Additional
views already publish `requested authority | mapped EXEC`; the primary view
previously published complete mapped protection. The primary and additional
paths are now semantically equivalent: publish only the requested data
authority, retain EXEC solely when the resolved mapping is executable, keep
the mapping incarnation, and leave exact dirty publication and SMC epoch exit
unchanged. Thus a READ invocation on an RW mapping cannot acquire WRITE, while
a READ or WRITE invocation on an executable mapping retains the executable
identity needed for invalidation.

| Capability | Retained C | Rust owner/status |
| --- | --- | --- |
| Requested data-access check | Resolver checks requested READ/WRITE | Projection `authority`; implemented |
| Complete executable identity | Snapshot view protection | Mapped protection contributes EXEC only; implemented |
| Mapping identity/lifetime | Snapshot plus mapping/JIT synchronization | Lease admission, transaction, and incarnation; implemented |
| Failed or cross-span store | Validate before mutation/fault | Native guard plus exact publication; unchanged |
| Completed executable write | CPU SMC range then dispatcher commit | Exact dirty range, token invalidation, epoch exit; unchanged |
| Partial REP store | Completed elements retained | x86 REP exact publication; unchanged |
| Teardown/fork/checkpoint | Mapping/JIT synchronization and repair | Lease lifetime and generation gates; unchanged |

Measured performance epoch delta: **0**. This authority correction did not run
or alter a performance benchmark and makes no performance claim. The focused
diagnostic assertion only proves that an executable write still produces its
single required public-epoch transition after permissions are narrowed.

## Executable certificate and publication-policy boundary

This is the smallest coherent plan for the existing native kernel and
`ProjectionLease`. It does not introduce another backend or revive the dormant
certificate fields piecemeal. No field may affect execution until its schema,
both architectures, lease decoder, and lifecycle tests exist in one clean tree.

### Values and owners

`src/runtime/hl-memory/src/mapping/projection.rs` owns
`WritePublication::{Exact, WholeView}` and a certificate containing the exact
guest interval and checked host base, separate requested READ/WRITE authority
and mapped execute identity, mapping incarnation, ledger and instruction
generations, stable view index, and policy. `WholeView` is legal only for a
host-coherent mapping whose backing owner proves that no executable alias exists
for the lease lifetime. File/shared mappings remain `Exact` until equivalent
cross-coordinator evidence exists. Eligibility evidence is lease-live and
revocable: an alias or protection transition changes generation and drains the
old lease before the proof can be used again. `ProjectionLease` continues to
retain checkpoint admission, the mapping transaction, storage, backing, and
every write reservation.

The size-qualified native mirror belongs in
`src/native/exec/include/executor.h`; zero means absent or `Exact`, never an
inferred permission. `src/containers/hl-engine/src/native/executor.rs` owns
the matching `RunCertificate` conversion and result validation.
`WritePublication` is `repr(u16)`, with `Exact = 0` and `WholeView = 1`.
Append a certificate pointer to `hl_native_run_request` only at offset 160
(`size == 168`); all older request sizes take the full guard. The pointed
size-qualified `hl_native_run_certificate` contains ABI, size, architecture,
`data_permissions: u32`, `mapped_executable: u32`, guest bounds and host base,
mapping incarnation/generation, instruction generation, authority
identity/generation, run generation, view index, write policy/reserved, and the
direct-token pointer. The record contains:

| Identity | Required invariant |
| --- | --- |
| guest bounds and host base | nonempty checked interval; compute offset as `guest - guest_first`, prove it is in range, then checked-add to `host_first`; both guest and host ends cannot overflow |
| data permissions | only requested READ/WRITE authority; never derive WRITE from mapped protection |
| execute identity | separate mapped EXEC bit used only for dirty/SMC classification |
| executor and architecture | match the owning executor instance and compiled guest/host architecture contract |
| source and mapping incarnation | equal source span, projection, request, cache identity, and live lease |
| mapping/instruction/run generations | live-checked against the lease, executable token, and invocation at every ingress; direct synchronization must not retain its current zero instruction epoch |
| authority identity/generation | match registered direct or projected run authority and cannot be zero |
| policy/view index | match the source view and exactly one retained reservation owner |

CPU scratch is declared only in `src/native/cpu/layout.tsv`, generated by
`src/native/cpu/generate.rs`, and consumed through generated C and Rust layouts.
It contains the immutable payload plus a release-published `certificate_token:
u64`; the token is written last and cleared first, before payload reuse.
Duplicate offsets are forbidden.

### Entry, chain, access, and publication

`src/native/exec/src/executor.c` validates immediately before native entry.
Publication ordering is payload initialization, release-store token, publish
the fault scope, then enter. Every AArch64 direct/indirect/return/self-loop
relocation and x86 live-chain ingress acquire-loads the token and authenticates
the complete record before using it. Failure first spills complete guest state,
returns to the dispatcher, and uses the current full guard. Exit ordering is
leave generated code, fully publish guest/fault/write state, unpublish the fault
scope, release-clear the token and payload, then permit lease teardown.

After authentication, AArch64 `guard.c`, `single.c`, `pair.c`, `ordered.c`,
`structure.c`, and `zero.c`, and x86 `projection.c`, `run.c`, and
`frontend/memory.c`, may replace view selection with certificate bounds and
permissions. Checked end construction remains per access unless the trace owns
a complete member envelope. Stores reserve capacity before the host instruction.
An operation that faults before its first write publishes nothing. A
partial-progress operation such as REP or a multiwrite fallback publishes every
completed range exactly once and rolls back only the uncompleted remainder.
Successful native stores publish afterward.

`Exact` returns owner-qualified ranges. `WholeView` returns its
generation-qualified view index once after the first successful store. Mixed
runs may contain both. Rust rejects duplicate, unknown, stale, out-of-view, or
policy-mismatched results and commits or rolls back each reservation exactly
once. Executable identity always forces exact ranges, SMC invalidation, and the
epoch exit.

Map, unmap, protection, remap, backing-alias, executable-generation, fork, and
checkpoint transitions first exclude new entry, drain admitted executions,
clear certificate tokens, invalidate affected cache records, relocations, IBTC
and direct/live chains, rotate every applicable generation, and only then
retire storage. Native executor fork repair and
`src/containers/hl-engine/src/native/fork_wire.rs` perform this reset for both
parent and child without serializing certificate state. Teardown removes native
reachability before dropping the lease.

### Clean committed stages

1. Add the Rust classifier and backing/coherence/alias tests, while every
   production view still emits `Exact`.
2. Append the versioned ABI and generated layout, validators, reset/fork, and
   C/Rust offset tests; no emitter reads it.
3. Publish and authenticate at outer entry and every existing direct/live-chain
   ingress; mismatch falls through to the full guard.
4. Enable certified reads on both architectures and prove stale-token, rotated
   authority, permission, cross-end, mutation, retired cache, fork, checkpoint,
   and teardown rejection.
5. Add mixed result encoding and exactly-once lease decoding while all
   classifications remain `Exact`.
6. Enable `WholeView` only for source-proven eligible mappings, then certified
   stores with unchanged pre-store capacity and post-success publication.

Each stage is verified from its exact clean commit and depends only on committed
predecessors. Old request sizes retain full guards and exact publication.
Unknown policies, absent appended fields, and zero or mismatched tokens fail
closed. Diagnostics remain append-only.

### Evidence and go/no-go

#### Dormant cache-certificate identity lifecycle audit

The read-only retained oracle was `/Users/x/dd/engine` at
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The lifecycle comparison covered
`src/translator/cache.c::{stw_before_translated,stw_after_translated,
stw_mapping_begin,stw_mapping_end,stw_checkpoint_arm,stw_checkpoint_wait,
stw_checkpoint_end,stw_after_fork}` and AArch64
`src/translator/guest/aarch64/translate.c::{emit_a64_soft_guard_begin,
emit_a64_soft_guard_end,aarch64_soft_tlb_miss}`. Retained per-thread cached
identity is cleared only while mapping exclusion has drained translated owners;
checkpoint captures no live fast-path identity, and fork reconstructs the
surviving thread registry before translated re-entry. The identifier is
task/registry state, never guest architectural state or serialized metadata.

The Rust/native prerequisite maps that ownership to one executor-local,
lock-free, nonwrapping issuer. The issuer is private and remains unconsumed in
production at this stage; direct tokens, cache entries, and CPU state receive no
nonzero identity. Zero is absent. `UINT64_MAX` is issued once, then the atomic
remains saturated and all later requests return zero without preventing ordinary
execution. Its executor-wide namespace is shared by both ISAs, so identities
cannot collide across architecture; the future immutable record must still
include architecture and must never infer it from the number. Fork
copies the monotonic counter but publishes no CPU identity; parent and child
processes remain separate identity domains. Reset, invalidation, exec,
checkpoint, and restore neither serialize nor reset it.

`src/native/cpu/layout.tsv` remains the sole CPU-layout source. It appends
`certificate_cache_identity` immediately before the release-publication token,
keeping the token last. AArch64 grows from 2336 to 2344 bytes (identity 2328,
token 2336); x86-64 grows from 1936 to 1944 bytes (identity 1928, token 1936).
Construction leaves both words zero, and execution bind/leave never writes the
dormant identity; token release-clear ordering is unchanged. This stage
does not add the identity to a run request, cache entry, relocation, IBTC,
generated instruction, guard, projection reader, diagnostic, or checkpoint.
It allocates no record and enables no fast path.

The current IBTC stores only target/body and therefore bypasses an admission
record; this is inventory for the later cache stage, not changed here. A future
identity may not reuse a slot within one cache generation, and rollover or fork
must exhaustively invalidate cache entries, relocations, and IBTC before reuse.
Both parent and child fork repair currently clear IBTC/cache reachability and
publish no CPU carrier. The appended eight-byte executor atomic consumes existing
padding on the verified AArch64 compiler (`sizeof(hl_native_executor)` remains
768 bytes, delta zero); CPU objects add eight bytes each. No token allocation or
run-path instruction is added, and activation generation remains a distinct
admission-lifetime value.

Differential tests compare retained C and Rust bytes, fault address/access/width,
partial progress, and executable invalidation for scalar, pair/vector, ordered,
structure, zeroing, x86 scalar/RMW/vector, and REP forms. Lifecycle tests cover
mixed primary/additional owners, full capacity before mutation, faulting first
store, completed store followed by interrupt/epoch exit, disjoint SMC aliases,
mprotect/map-fixed/remap, checkpoint/restore, token
ABA attempts, mapping and instruction generation rollover, partial REP and
multiwrite completion, retired cache entry, parent/child/nested fork, restore
ABA, concurrent mapping/protection pressure, and exactly-once rollback. A
table-driven mismatch suite changes each certificate field independently,
including executor, architecture, source, view index, policy, all identities and
generations, each permission bit, zero tokens, guest/host end overflow, and
release/acquire publication races; every case must fail fully spilled and leave
memory and publication state unchanged.

Performance uses separately built exact clean release artifacts, the identical
guest and manifest, divisor 20, one warmup, at least five alternating pairs, one
isolated CPU/job, and typed native diagnostics. Memory and float must preserve
checksums and semantic counters. The attribution baselines are memory
28,991,955 full guards / 14,516,893 dirty publications / 3,628 generated
fallbacks and float 87,859,588 / 29,213,509 / 8,844, both with zero fast guards.
Go requires nonzero useful certified hits, full-guard reduction exactly matching
those hits, no median regression in either phase, no regressing pair beyond the
predeclared noise bound, and a paired confidence interval excluding the prior
3.758%, 4.39%, and 4.72% certificate regressions. A 10% improvement remains the
promotion target, not a substitute for the no-regression and hit-coverage
gates. Any stale certificate,
premature publication, lost completed write, SMC under-invalidation, unexplained
counter drift, or material code-cache regression is a no-go and full revert.
## Consolidated AArch64 write-publication evidence (2026-08-05)

## AArch64 cached writable-view audit

### Retained C oracle

The read-only oracle was `/Users/x/dd/engine` at
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The complete write/projection
path studied was:

- `src/translator/guest/aarch64/translate.c`: `emit_a64_soft_guard_begin`,
  `emit_a64_soft_guard_end`, `emit_a64_soft_exit_site`, `emit_fold_mem`,
  `aarch64_soft_tlb_miss`, `aarch64_soft_tlb_span`,
  `aarch64_soft_prepare_bounce`, `aarch64_soft_bounce_commit`, and the SMC
  range queue. One CPU owns its cached interval and host delta. A hit performs
  no lock or callback; a discontinuous store validates every span, blocks host
  signals, writes a bounded bounce, and scatters only after success.
- `src/translator/guest/aarch64/cpu.h`: the CPU owns the cached interval,
  permissions, miss metadata, bounce state, signal-mask storage, and SMC
  ranges for its thread lifetime.
- `src/translator/guest/aarch64/dispatch.h`: `R_SOFTMISS`, `R_SOFTSPAN`, and
  `R_SOFTCOMMIT` retry/fault ordering. A failed resolution becomes an
  architectural fault; a bounced store is committed before another guest
  instruction can observe it.
- `src/translator/guest_memory.c`: bind, executable-span resolution, data pin,
  unpin, read, and write entry points. Pins own borrowed storage only until
  unpin.
- `src/translator/cache.c`: `stw_register`, `stw_unregister`,
  `stw_mapping_begin`, `stw_mapping_end`, checkpoint admission, and fork
  repair. Mapping mutation stops admitted CPUs and clears cached intervals
  before snapshots or backing can retire.
- `src/linux_abi/thread.c`: GNA/GRO generation readers and writer locks, BUS
  transition ordering, and file-map publication. Generation readers retry
  concurrent mutation and conservatively fail closed rather than authorize a
  stale write.

The retained ordinary hit path does not maintain Husklet's exact per-store
journal. Executable and discontinuous writes are nevertheless published only
after success. Linux and macOS differ in the direct host-range probe; the
generated AArch64 hit sequence and stop-the-world lifetime rule are shared.

### Husklet ownership and correction

Husklet retains stronger authority. `ProjectionLease` owns checkpoint
admission, the mapping transaction, projected storage, generation, backing,
and write reservations. `run_view_publish` release-publishes at most four
immutable views and their exact publication identities. `guard.c` must reserve
journal capacity before a host store and commit the exact range only after the
store succeeds. Mapping mutation, fork, and teardown cannot reclaim the
storage while that lease remains live.

Before this change, `write_cache` handled an alternation between already
projected writable views by overwriting `memory_first`, `memory_last`, delta,
permissions, write policy, and write index, then retrying the guard. If the old
active view owned completed writes, that mutation happened before
`hl_a64_guard_write_begin` could archive them. Guest bytes were correct while
the returned exact journal could lose or misattribute the preceding owner.

The cached selector now keeps only the selected immutable-view index in x9,
then enters one common activation path. A nonempty old interval first checks
the 16-record capacity and records its exact view and written range. Only then
does activation replace the active view fields and retry the original guard.
Overflow restores NZCV and x9 and exits for epoch service before the guest
store. An empty journal skips archival. Selected-view publication still comes
from the token-acquired immutable payload, and no pointer survives the run
lease.

All current native writer families use this central sequence: scalar integer
and vector stores (1--16 bytes), integer/vector pairs (up to 32 bytes), ordered
stores (1--8 bytes), AdvSIMD structures (up to 64 bytes), and 64-byte DC ZVA.
Exclusive and atomic read-modify-write families remain deliberately declined
to fallback; this change does not partially admit them.

### Verification and measurement limits

On Linux AArch64, warning-strict direct executables built the complete native
source set plus both entry assemblies. `aarch64_single` passed. The complete
`aarch64_trace` executable passed after applying only a temporary test-harness
correction from a pre-existing seven-instruction build limit to the nine
instructions that its adjacent assertion already expects; that unrelated
one-line correction is not part of this commit. The new alternating-write
assertions prove three successive view switches retain the previous exact
owners and ranges while the final interval remains live.

No runtime speed claim is made. At verification time `/tmp` had only 1.1 GiB
free and the shared repository target occupied 54 GiB while other lanes owned
build execution. A clean diagnostics-off retained-C/Rust/native benchmark
could not be built without either violating build ownership or risking the
host's remaining disk. The emitted cache-hit activation is centralized rather
than duplicated four times, but static size is not a substitute for an engine
A/B. The existing scalar-conversion benchmark remains the required clean-tree
performance gate after integration.

## AArch64 dirty-record coalescing audit

### Retained-C oracle

The read-only oracle was `/Users/x/dd/engine` at
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The complete write-observation
path inspected for this lane was:

- `src/translator/guest/aarch64/translate.c`: translated load/store entry
  points and the successful-store publication boundary;
- `src/translator/guest/aarch64/cpu.h` and `dispatch.h`: soft-TLB ownership,
  dispatcher state, and translated-block exits;
- `src/translator/guest_memory.c`: guest-view resolution, permission checks,
  partial failure, and write observation;
- `src/translator/cache.c`: translated-code invalidation and cache lifetime;
- `src/linux_abi/thread.c`: task ownership, teardown, and signal interaction.

The retained implementation owns one current soft-TLB view. It publishes
self-modifying-code and bounce-buffer effects only after a successful store;
failed stores publish nothing. Its state is task-owned, and dispatcher/cache
locks do not span guest memory access. Architecture-specific address recovery
is confined to AArch64 translation and dispatch. There is no retained exact
per-store journal, so the Rust journal is an implementation mechanism rather
than guest-visible behavior.

### Rust capability mapping

`guard.c` owns generated successful-store accounting, cached-view switches,
and epoch exits. `projection.c` owns dispatcher-side view changes and final
journal flushing. `trace.c` owns admission when an existing journal is already
full. `executor.c` consumes the bounded records after native execution.

Before this change all archive paths appended. Alternating between two cached
owners therefore exhausted the 16-record journal even when every archived
interval exactly overlapped an earlier interval for that owner. The resulting
epoch exits were bookkeeping pressure, not a semantic boundary present in the
retained implementation.

All three admission/publication paths now use the same rule: an active exact
interval may reuse a record only when the owner bounds are identical and the
dirty ranges overlap or touch. Coalescing expands only the dirty interval and
never changes its owner. Otherwise the bounded append/overflow behavior is
unchanged. Generated code archives a completed old interval before attempting
a store through a different owner; the possibly faulting new store therefore
cannot publish itself early. Guest register x9, NZCV, and the selected cache
slot survive the cold bounded scan.

### Evidence

The regression starts with all 16 records occupied, including records for two
alternating owners. Two stores switch owners before a syscall. The old code
could not archive the first completed interval and exited at the capacity
boundary; the candidate merges both completed intervals, reaches the syscall,
does not set overflow, and retains 16 records. The existing alternating-owner
case also verifies that a changed owner extent is not falsely merged.

On Linux AArch64, direct native executables compiled with
`-std=c11 -Wall -Wextra -Werror -O2` passed:

- `aarch64_trace` with the repository's pre-existing SIMD trace-limit typo
  corrected locally from 7 to 9 for the run and reverted afterward;
- `aarch64_single` without test changes.

Performance timing is intentionally deferred to a coordinated quiet window.
At `7ac681960`, the testing benchmark runner re-enables diagnostics whenever
native execution is selected, so authoritative diagnostics-off A/B must invoke
the two clean `hl-engine` binaries directly after one diagnostics-on provenance
row. The comparison target is the recorded 36.297x baseline.

### Baseline re-audit at `cf15cdd33`

This lane re-audited the exact baseline instead of duplicating the active
`agent/a64-projection-cert`, `agent/a64-ingress`, or
`agent/a64-ingress-cert` branches. The retained oracle remained read only.
The exact C entry points followed were `translate.c::{emit_a64_soft_guard_begin,
emit_a64_soft_guard_end,aarch64_soft_tlb_miss,aarch64_soft_tlb_span,
aarch64_soft_prepare_bounce,aarch64_soft_bounce_commit}`, `dispatch.h`'s
`R_SOFTMISS`, `R_SOFTSPAN`, and `R_SOFTCOMMIT` transitions, `cpu.h`'s
task-owned soft-TLB/bounce fields, and `cache.c::{stw_before_translated,
stw_after_translated,stw_mapping_begin,stw_mapping_end,
map_invalidate_source_ranges}`. Rust/C owners followed were
`guard.c::{write_cache,hl_a64_guard_begin,hl_a64_guard_written}`,
`projection.c::{mergeable,flush_dirty,hl_a64_projection_resolve}`,
`trace.c::{hl_a64_trace_loop_preflight,hl_a64_trace_certificate_check}`, and
`executor.c::{run_aarch64,active_view_publish,hl_a64_run_view_publish}`.

The retained CPU owns one tuple for its task lifetime. Registration and
mapping mutation use registry/JIT locks to park peers and clear tuples before
backing retirement; generated hits allocate and lock nothing. Misses record
PC, address, width, and access before dispatch. Cross-span writes block
signals, use bounded bounce storage, restore the prior mask, and commit only
after the copy. Permission failures and synchronous faults publish nothing;
teardown unregisters before reclamation. POSIX STW signalling and the macOS
host-range probe are host branches; emitted AArch64 hits are host-neutral.

Rust pins projected storage with the run-scoped `ProjectionLease` and
authenticates mapping/authority at admission. Its fixed 16-record journal is
CPU-owned and allocation-free. Stores reserve archive capacity before
mutation, record only after success, and force an epoch exit for executable
owners. Owner-qualified overlap/touch coalescing and full-journal preflight
are already present. Rust validates records before reconciliation,
executable/exclusive invalidation, and reservation commit; drop rolls back.

| Capability | Retained C | Rust at `cf15cdd33` | Status |
| --- | --- | --- | --- |
| Writable-view identity | task tuple retired under STW | lease-pinned bounds plus incarnation/authority | implemented |
| Successful/faulting write | post-store commit/no fault commit | post-store exact interval/no fault record | implemented |
| Repeated owner accumulation | coarse current tuple | bounded owner-qualified merge | implemented |
| Capacity/executable write | coarse invalidation | pre-mutation epoch/overflow; exact epoch exit | implemented |
| Cross-span discontinuity | signal-masked bounce | dispatcher fallback | divergent but safe |
| Authenticated certificate | registry-cleared tuple | `read_token`/`read_incarnation`/`read_views` window | implemented |
| Fork/teardown retirement | registry clear/removal | lease plus instance execution gate | implemented for live path |

Mechanically, nothing ever assigned a nonzero `certificate_valid` or
`certificate_delta`, and a two-word `{valid, delta}` payload could not have
authenticated bounds, permissions, mapping incarnation, authority identity, or
lease generation. Both words have been removed; the release-published
`read_token`/`read_incarnation`/`read_views` window is the live authenticator. A coherent certificate therefore belongs to the separate
ingress/lifecycle work and requires mutation, fork, direct-chain, permission,
fault, and teardown tests. It is not a safe dirty-journal-only edit.

## AArch64 contiguous dirty publication audit

### Retained C oracle and Rust mapping

The read-only oracle was `/Users/x/dd/engine`. The complete relevant path was
`src/translator/guest/aarch64/translate.c` (`emit_a64_soft_guard_begin`,
`emit_a64_soft_guard_end`, `aarch64_soft_prepare_bounce`, and
`aarch64_soft_bounce_commit`), `dispatch.h` (`R_SOFTTLB`, `R_SOFTSPAN`, and
`R_SOFTCOMMIT` ordering), `cpu.h` (soft-TLB span, bounce and vector-dirty
ownership), and `src/translator/cache.c` (mapping stop-the-world invalidation).
The retained fast path caches one mapping-qualified interval. Sequential stores
inside it do not repeatedly republish its invariant owner; executable/bounce
publication occurs only after successful stores, and discontinuous or faulting
accesses enter the cold path.

Husklet's stronger exact owner is `guard.c` plus `projection.c`, the executor's
generation-qualified run views, and `ProjectionLease::publish_written_ranges`.
The generated guard records an exact successful-store interval only after the
host instruction. A first store establishes `dirty_view_first/last`,
`memory_written`, and the executable bit. A disjoint store archives that tuple
before starting another. An overlapping store merges exact bounds. Mapping,
checkpoint and backing lifetime remain retained by the projection lease.

Before this change, the already-specialized contiguous path extended
`dirty_last` and then redundantly reloaded and rewrote the unchanged view owner,
written bit and executable bit. The fast continuation now extends only the
exact interval end, while retaining both completed/merged diagnostics. It is
reachable only after `dirty_first != UINT64_MAX`, `address == dirty_last`, and
the active `memory_first/last` exactly matches `dirty_view_first/last`. The last
qualification is load-bearing: cached run views may be adjacent in guest space
while owning different projections. Such a transition archives the prior exact
record and establishes the new owner. A fault cannot reach the continuation
because this emitter runs after the guest store. No range is widened, no view is
inferred, and coarse publication is not used.

### Verification and performance

The focused AArch64 single-store executable initializes a qualified interval,
performs the adjacent store, and proves exact end extension plus unchanged view,
written and executable ownership. Further regressions make distinct views
ascending-adjacent, descending-adjacent, and guest-overlapping, and prove the
prior owner/range is archived before the new exact interval is established.
View mismatch branches directly to archival; only address non-contiguity enters
the range-relation logic, so its comparisons never consume view-comparison
flags. The suite also retains the existing pre-store
journal-capacity refusal and cross-window fault tests. The warning-strict native
executor dirty-journal tests passed, as did the standalone warning-strict
`aarch64_single.c` executable.

One release runner and guest drove both the scalar-conversion parent and this
candidate with typed native diagnostics, divisor 100, and three repetitions.
The parent median was 1,805,477 us; the final flag-safe, view-qualified candidate
median was 263,392 us (261,143--263,452), an 85.41% reduction. Checksum
23,027,281,045 was identical. Candidate counters were exact across all three
repeats: 43,392,048 completed instructions, 16,158,994 exact guards, 5,375,202
committed stores, 5,373,557 merged stores, zero journal overflows and 1,630 guard
fallbacks. They intentionally differ from the unqualified prototype: adjacent
distinct views now archive separate records rather than incorrectly merging by
guest address alone, allowing the complete workload to remain native.

A CPU-17 matrix using the same candidate measured host-native at 5,292 us,
retained C at 5,465 us, and Rust at 971,404 us, all with the same checksum.
Host load was high (28.78 one-minute), so these absolute figures locate the
remaining 183.56-times native gap rather than claim a stable release result.

## AArch64 projection-lease generation audit

### Scope and exact trees

This bounded implementation-prerequisite audit used Husklet baseline
`cf15cdd33`, retained read-only oracle `/Users/x/dd/engine`, certificate
candidate `agent/a64-ingress-cert` at `a21de7906`, and dirty-publication
candidate `agent/arm-dirty-coalesce` at `ad2a377c0`.  The retained tree was
not modified.  No production change is made because the independently
comparable value has two owners which cannot be split without creating a
false lifetime proof: Rust owns the live projection, while native execution
owns fork repair and every translated ingress.

### Retained C and assembly lifecycle

The complete relevant retained path was inspected in:

- `src/translator/guest/aarch64/translate.c`:
  `emit_a64_soft_guard_begin`, `aarch64_soft_tlb_miss`,
  `aarch64_soft_tlb_span`, `aarch64_soft_prepare_bounce`, and
  `aarch64_soft_bounce_commit`;
- `src/translator/guest/aarch64/dispatch.h`: the `R_SOFTMISS`,
  `R_SOFTSPAN`, and `R_SOFTCOMMIT` transitions;
- `src/translator/guest/aarch64/cpu.h`: task-owned `soft_page`, exclusive
  `soft_limit`, `soft_delta`, protection, pending-write, and bounce state;
- `src/translator/cache.c`: `map_invalidate_source_ranges`, `stw_register`,
  `stw_unregister`, `stw_before_translated`, `stw_after_translated`,
  `stw_mapping_begin`, and `stw_mapping_end`.

One registered task owns its soft-TLB identity from registration through
unregistration.  Guard hits check a complete half-open interval and required
permission before applying the host delta.  Miss and span exits retain exact
PC, address, width, and access direction.  A discontinuous write is validated
and copied through bounded bounce storage while signals are blocked, and is
published only by the successful commit transition.  Mapping mutation holds
the JIT and registry gates, parks translated peers, invalidates their cached
source ownership before backing reclamation, refreshes the mapping view, and
then wakes them.  Registration and teardown occur under the registry lock.
The generated interval check is host-neutral; POSIX stop-the-world signalling
and signal masking, and the macOS direct-range probe exception, are the
host-specific branches.

### Rust/native ownership matrix

| Required capability | Current owner at `cf15cdd33` | Result |
| --- | --- | --- |
| Stable mapping/backing and checkpoint exclusion | `hl_memory::ProjectionLease` transaction and activity admission | implemented for one synchronous run |
| Mapping, instruction, and process incarnation | `ProjectionGeneration` | implemented, but none identifies the individual lease |
| Run authority and retirement | `DirectAuthorityLease`, native direct-token generation/identity, and `hl_native_execution_enter/leave` | implemented only for direct mode |
| Independently comparable lease generation | no owner | missing |
| Clear on normal return and fault-publication failure | `run_aarch64` clears active view/token state | implemented for existing fields; no lease generation exists |
| Mutation exclusion | mapping transaction plus native mutation admission | implemented while the Rust lease is borrowed |
| Fork-child retirement | native executor fork repair/cache reset | cannot be proved by an `hl-memory` counter copied into the child |
| Direct entry, direct chain, and IBTC authentication | common translated body ingress in `trace.c` | candidate-only and lacks lease generation |
| Permission/incarnation rejection | projection resolver and candidate certificate checks | implemented on slow path; candidate-only on ingress |
| Write-owner/dirty retirement | `ProjectionLease::publish_written_ranges`; guard/projection journal | dirty-coalesce candidate changes the same ingress/guard state |
| Executor and CPU teardown | Rust handle drops plus native execution/mutation gates | no generation-specific assertion or clearing test |

`ProjectionLease::generation()` currently returns mapping incarnation, mapping
ledger generation, and instruction generation.  Adding a fourth integer from
an `AtomicU64` in `hl-memory` would not authenticate lifetime: after `fork()`
the child inherits the same nonzero atomic and projected host addresses even
though only native fork repair knows that their execution authority must be
retired.  Clearing only the CPU copy on return is also insufficient because a
direct chain or IBTC entry consumes CPU state before returning to Rust.

Conversely, minting only a native counter would not prove that the
`ProjectionLease` still owns checkpoint admission, the mapping transaction,
host projection objects, and write reservations.  A sound contract therefore
requires one cross-boundary activation operation which borrows the Rust lease,
publishes a nonzero non-wrapping generation into the native run request, and
registers that generation with the executor's fork/mutation lifecycle.  Every
translated body ingress must compare it with the active generation.  Native
return, fault-publication failure, mutation admission, fork repair, and destroy
must retire it before Rust drops the lease or reclaims a projection.

### Candidate interaction and blocker

`a21de7906` adds bounds, read permission, incarnation, and authority checks at
the shared body entry, so it identifies the correct direct-chain/IBTC
authentication point.  It publishes no independently comparable lease value.
`ad2a377c0` changes guard and projection dirty-owner transitions.  Both were
built on different pre-baseline histories and overlap `guard.c`, `trace.c`,
CPU layout, executor state, and trace tests; neither is a prerequisite series
for `cf15cdd33`.

The isolated lane cannot truthfully implement or test the requested contract
without owning both the Rust activation API and native fork/mutation registry,
which necessarily touches the same semantic surfaces as the two active
candidates.  A memory-only generation would pass ordinary return tests while
remaining falsely live in a fork child; a CPU-only generation would pass
direct-chain tests without retaining backing.  Either would manufacture the
authority token rejected by the integration audit.

The next coherent lane must own these changes together on the current
baseline: typed Rust lease activation/retirement; a nonzero exhaustion-safe
generation in `hl_native_run_request`; executor registration and fork repair;
CPU active/certificate generation; common-body ingress comparison; and tests
covering normal return, fault-publication failure, permission and incarnation
changes, mapping mutation, fork child, direct entry, direct chain, IBTC,
rollover exhaustion, and executor/CPU teardown.  Only after that proof should
the read-only ingress candidate be reconstructed, followed independently by
dirty coalescing.  Because this audit changes no production hot path, no
pinned performance comparison is claimed.

## AArch64 certificate integration audit

### Scope and exact trees

This read-only integration audit used baseline `cf15cdd33`, retained-engine
oracle `/Users/x/dd/engine`, and these candidate tips:

| Candidate | Tip | Merge base with baseline | Shape |
| --- | --- | --- | --- |
| `agent/a64-projection-cert` | `b0ee7bd54` | `a0ff93cd4` | documentation only; its code change was reverted |
| `agent/a64-ingress` | `43c1eb2d3` | `ad5dc3b42` | bounds/permission/incarnation/authority checks on each guard |
| `agent/a64-ingress-cert` | `a21de7906` | `ad5dc3b42` | ingress authentication, read-only member fast path |
| `agent/arm-dirty-coalesce` | `ad2a377c0` | `7ac681960` | dirty-owner journal coalescing |
| dirty-path re-audit | `2593629d8` | n/a | documentation; audits baseline `cf15cdd33` |

The candidates do not form a cherry-pick series. `git merge-tree` reports
content conflicts for both certificate tips together, and for either
certificate tip combined with dirty coalescing. The conflicts include
`guard.c`, `executor.c`, CPU layout/initialization, and AArch64 trace tests;
they are semantic conflicts rather than mechanical context drift.

### Retained C/assembly oracle

The retained tree was never modified. The complete relevant ownership path was
followed through:

- `src/translator/guest/aarch64/translate.c`:
  `emit_a64_soft_guard_begin`, `emit_a64_soft_guard_end`,
  `aarch64_soft_tlb_miss`, `aarch64_soft_tlb_span`,
  `aarch64_soft_prepare_bounce`, and `aarch64_soft_bounce_commit`;
- `src/translator/guest/aarch64/dispatch.h`: the `R_SOFTMISS`,
  `R_SOFTSPAN`, and `R_SOFTCOMMIT` transitions;
- `src/translator/guest/aarch64/cpu.h`: task-owned interval, delta,
  protection, bounce, and pending-write fields;
- `src/translator/cache.c`: `map_invalidate_source_ranges`,
  `stw_before_translated`, `stw_after_translated`, `stw_mapping_begin`, and
  `stw_mapping_end`.

The retained task owns one soft-TLB tuple for its lifetime. A hit checks the
complete half-open interval and permissions before adding the host delta.
Misses spill exact PC/address/width/access and return through the dispatcher.
Discontinuous cross-span writes use bounded bounce storage, block signals while
copying, restore the signal mask, and publish only after successful commit.
Mapping mutation holds the mapping/JIT lifecycle gates, parks translated peers,
invalidates source ranges, and clears/refreshes cached ownership before backing
can retire. POSIX stop-the-world signalling and macOS host-range probing are
host-specific; emitted AArch64 interval hits are host-neutral. Faults and
permission failures never publish a successful write.

### Rust mapping and integration decision

| Required property | Baseline owner | Candidate result | Decision |
| --- | --- | --- | --- |
| Half-open bounds and overflow rejection | generated `guard.c` selectors | both certificate tips add checked bounds | conceptually required |
| Read/write permission | generated `guard.c` | ingress checks both; ingress-cert deliberately admits reads only | prefer read-only first |
| Mapping incarnation | `run_aarch64` active view | both carry it | required |
| Run authority | direct token / mapping epoch and execution admission | both carry it | required but not sufficient alone |
| Lease generation | Rust `ProjectionLease` lifetime, not CPU layout | neither candidate carries an independently comparable generation | unresolved |
| Write-owner transition | `memory_*`, dirty journal, projection reconciliation | ingress clears on cache owner switch; ingress-cert excludes writes | do not merge with coalescing mechanically |
| Fork/mutation/teardown retirement | execution gate plus lease | tested indirectly, no certificate-specific generation/retirement proof | unresolved |
| Direct-chain/IBTC ingress | trace entry layout | ingress-cert adds authentication at the shared body entry | promising, needs exact entry-path proof |

`agent/a64-projection-cert` is coherent and cherry-pickable only as historical
documentation: it explicitly reverted the attempted implementation. The two
code tips are alternative experiments. `agent/a64-ingress-cert` is the safer
base for future work because it constrains the optimization to reads and makes
trace ingress the authentication boundary. It must not be called merge-ready:
its CPU certificate contains bounds, permissions, incarnation, and authority,
but no independently authenticated lease generation. `agent/a64-ingress`
widens the optimization to writes and overlaps dirty-owner state transitions,
so it must follow, not precede, a proved read-only lifecycle design.

Recommended order:

1. land/rebase the documentation audits;
2. define a run-scoped, nonzero, rollover-safe lease generation at the Rust/C
   boundary and prove clear-on-fault, return, mutation, fork, and teardown;
3. rework the read-only ingress design on top of the current baseline and prove
   direct entry, direct chain, IBTC, permission change, incarnation change, and
   stale-generation rejection;
4. integrate dirty coalescing independently and re-run its full-journal and
   owner-switch cohort;
5. only then consider authenticated writes, with pre-store reservation and
   post-store publication evidence.

No production implementation was made in this lane. Adding a field without a
defined Rust lease-generation owner and rollover/retirement protocol would
manufacture an authority token rather than authenticate backing lifetime.

## AArch64 projected-view cache audit

This audit selects the next generic AArch64 performance mechanism without
changing production behavior.  The Husklet source revision is
`c80934c86a997ace195c660ef1c2fa6b8f4eb38a`; the retained read-only oracle is
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`.  The retained checkout has an
unrelated deleted packaging README and an untracked `.claude/` directory, so
its revision identifies source rather than claiming a clean checkout.

### Selection evidence

The exact benchmark source `tests/bench/combined/main.c` and retained
`../engine/tests/perf/combined_bench.c` both have SHA-256
`ec97f6f5c598f6fc229231dbf4751fb298ebaf1ae04c530d8aecbc7a1ec926af`.
The pinned measurements in `PERFORMANCE.md` show that the AArch64 memory phase
is hundreds of times slower than retained C.  Runtime counters narrow the hot
domain further:

- vector pair loads and stores execute 17,149,757 of 17,150,872 guards
  (99.9935%);
- 151,095 fallback boundaries contain 150,420 native operand-cache hits but
  only 666 resolver callbacks (0.44%); and
- admitting more vector operations while retaining the complete per-access
  guard made guest time 9.95% slower.

Consequently the next high-value mechanism is a generation-qualified,
authority-bound projected-view cache with a short flag-free hit path and one
shared cold resolver.  More opcode admission, more projection slots, and a
one-base range certificate are contradicted by the measured workload.

### Retained implementation audit

The complete retained path studied was:

- `../engine/src/translator/guest/aarch64/cpu.h`, `struct cpu`: owns one
  thread's `soft_page`, exclusive `soft_limit`, `soft_delta`, permissions,
  miss metadata, cross-page span state, and bounce buffer.  The fields live
  for the CPU/thread lifetime and are not process-global.
- `../engine/src/translator/guest/aarch64/translate.c`,
  `emit_a64_soft_guard_begin`, `emit_a64_soft_guard_end`,
  `emit_a64_soft_exit_site`, `a64_fold_mem_offset`, and `emit_fold_mem`: emit
  a flag-free interval and permission hit test, add the cached host delta, and
  join cold misses at a shared block exit.  Every access still owns exact PC,
  width, direction, scratch restoration, and fault metadata.
- The same file, `aarch64_soft_tlb_miss`, `aarch64_soft_tlb_span`,
  `aarch64_soft_prepare_bounce`, `aarch64_soft_bounce_commit`, and
  `aarch64_soft_span_copy`: resolve a miss, reject protection failures,
  preserve discontinuous cross-page semantics with a bounded bounce, block
  signals across a bounced store, publish SMC ranges after success, and
  restore the prior signal mask on commit.
- `../engine/src/translator/guest/aarch64/dispatch.h`, the `R_SOFTTLB`,
  `R_SOFTSPAN`, and `R_SOFTCOMMIT` dispatch cases: retry only after a valid
  resolution, route unresolved accesses to the architectural fault path, and
  complete bounced writes before another guest boundary.
- `../engine/src/translator/cache.c`, `stw_register`, `stw_unregister`,
  `stw_mapping_begin`, and `stw_mapping_end`: own registry identity, locking,
  and teardown.  Mapping mutation holds the JIT and thread-registry locks,
  stops translated peers, clears every registered CPU cache before retired
  snapshots or backings can be reclaimed, refreshes the conservative VMA hull
  after publication, then releases the gate.  Registration seeds the current
  generation; unregister removes the CPU pointer while holding the registry
  lock.

The hit path itself takes no lock and performs no host call.  Its lifetime is
safe only because mutation and teardown are serialized by the stop-the-world
registry.  Miss resolution observes the immutable logical-VMA snapshot.  A
cross-page partial store is never published as wholly written: discontinuous
storage is validated first, copied through the bounded bounce, then committed.
Protection failure remains a guest fault; allocation or registry failure does
not become a permissive hit.

The emitter is AArch64-specific.  Linux and macOS differ in the direct-host
range probe (`hl_host_range_mapped` is skipped on macOS), while the generated
hit sequence is host-OS neutral.  POSIX signal masking is part of the cold
discontinuous-store path.  Cache invalidation and refresh are enabled through
the guest ABI's `G_SOFT_TLB_CLEAR` and `G_SOFT_TLB_REFRESH` hooks.

### Husklet mapping and gaps

| Retained capability | Current Husklet owner | Status |
| --- | --- | --- |
| Per-thread cached interval, delta, permissions | generated `hl_native_aarch64_cpu` `memory_*` and `read_views` | divergent: four-view linear selector is emitted at every access |
| Flag-free hit test and delta add | `src/arch/aarch64/guard.c` `legacy_begin`, `read_cache`, `write_cache` | divergent: saves/restores NZCV and may scan four views |
| Cold bounded resolver | `src/arch/aarch64/projection.c` `hl_a64_projection_resolve` | implemented for a borrowed projection, but exits through Rust per miss and is not a shared in-block stub |
| Authority and generation authentication | `src/executor.c` direct token/admission plus generated `read_token`/`read_incarnation` | implemented for one synchronous projection lease |
| Exact post-store dirty publication | `guard.c` `hl_a64_guard_write_begin`/`hl_a64_guard_written` and `projection.c` `flush_dirty` | implemented; must remain per successful store |
| Exact per-access provenance | AArch64 frontend/trace provenance and fault reconstruction | implemented; cannot be coarsened to a page or trace |
| Mutation-time invalidation before backing reclamation | executor mutation admission and projection lease ownership | missing for a persistent per-thread last-view cache |
| Thread registry and fork/teardown repair | executor admission/fork code and fault thread registry | divergent: no registry currently binds cached projected backing lifetime to every native CPU |
| Discontinuous cross-page bounce and atomic commit | no equivalent projected-view cache path | missing |
| Shared cold miss stub | guard fallback plus Rust run loop | missing |

The current projection is a bounded borrowed capability.  Caching its host
delta beyond the authenticated run without a registry-mediated lease would
permit use-after-retirement of host backing.  Merely adding `last_first`,
`last_last`, and `last_delta` fields, or choosing `memory_*` before the existing
four-view scan, would therefore weaken the ownership contract even if it made
the benchmark faster.

### Required experiment

No production change is accepted by this audit.  A valid A/B candidate must
first add one coherent mechanism that:

1. binds the cached interval to executor identity, projection authority,
   mapping incarnation, and an explicit live lease;
2. invalidates every admitted CPU before mapping replacement, token retirement,
   fork-child repair, cache rollover that changes authority, or destroy can
   reclaim backing;
3. retains a flag-free fast hit for pair-vector accesses regardless of base
   register, exact per-access provenance, pre-store journal reservation,
   post-store exact-range publication, and executable-write epoch exit;
4. sends overflow, permission mismatch, adjacent discontinuous views, stale
   identity, and cold misses through one bounded resolver without converting a
   valid partial operation into an all-or-nothing write; and
5. proves mutation races, stale tokens, fork, cross-page faults, discontinuous
   stores, SMC, and teardown before timing the unchanged combined benchmark.

Instrumentation must be typed per executor and disabled by default.  At
minimum it must count hit, cold miss, stale-identity rejection, cross-span,
resolver retry, and access form without changing emitted control flow in the
uninstrumented A/B binaries.  The benchmark must consume and validate the
guest checksum, report native diagnostics, alternate baseline/candidate on a
pinned CPU, and use at least five samples after warmup.  Generated code must
execute through the real engine; a standalone emitter microbenchmark is
optimizer-invalid evidence for this mechanism.

### Attempted measurement and resource bound

The host was Linux AArch64 with 18 logical CPUs.  At audit start it had about
12 GiB available RAM, 20 GiB free swap, 111 GiB free on `/Users`, and 6.4 GiB
free in `/tmp`; load average was 3.83/5.21/6.82.  An exact-tree release run was
started with:

```text
CARGO_BUILD_JOBS=18 cargo run --locked -p testing --bin testing --release -- \
  bench combined --isa arm64 --jobs 1 \
  --results target/testing/a64-view-audit.tsv
```

It was cancelled during dependency compilation when concurrent work reduced
`/tmp` free space to 4.2 GiB, below the manager's 6 GiB guard.  No benchmark
sample ran and no timing claim is made.  Existing exact-tree measurements rank
the mechanism; a reproducible before/after result must wait for adequate
scratch space rather than contend with active guest builds.
### Slot-zero exact A/B measurement

The exact comparison used parent `3a982426e1a9c4c75319844b903703ac8250af1c`,
candidate `817818b5a71cb3c1faf008dc3176f50afb87849a`, and retained C
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. Each Rust tree was built alone
with `CARGO_BUILD_JOBS=18 cargo build --locked --release -p testing --bin
testing`. The resulting `testing` SHA-256 values were respectively
`9ab6d567e4886c8d414cec4f9f7b1bbe83f3f8f5adec8d823992fbb2d3062334` and
`f28f37528bd21269916b95fd36791c429b62107c1b6c1e3258920061f0ceec65`.
The combined guest source hash was
`ec97f6f5c598f6fc229231dbf4751fb298ebaf1ae04c530d8aecbc7a1ec926af` in
both repositories. The retained C engine and guest hashes were
`9c2701b36a46050909b12498eb0b47673f301bcb35d57e552d343b029cf3a67a`
and `07756eb451ec3c063a6ffed129db76b8702d5a37be8ba9cbf79eb77944d052ee`.

The Linux AArch64 host had 18 logical CPUs. Runs were pinned to CPU 17 with
the `performance` governor; CPU 0 reported 2 GHz. At start, load was
2.86/4.80/5.45, available RAM was 15 GiB, swap was 3.7/28 GiB used, `/Users`
had 108 GiB free, and `/tmp` had 12 GiB free. During the run `/tmp` remained
above 7.3 GiB; final load was 1.15/3.73/4.94 and no zombies were observed.

Each row used one warmup followed by one measured repetition of the memory
phase at divisor 20, alternating parent then candidate, with:

```text
taskset -c 17 env \
  HL_COMPAT_ENGINE_OPTIONS='HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1' \
  target/release/testing bench combined --isa arm64 --jobs 1 --results <path>
```

The temporary one-repetition manifest hash was
`ac605af602c70db30b844507e5f51609bf6b174d295bbe71bbef14520219adcb`;
the checked-in manifest and golden files were restored after measurement.
Both binaries reported `Execution { native: true, diagnostics: true }`, output
checksum `36526`, and the same nonzero diagnostics on every measured row:

```text
hl-native: runs=900 builds=229 hits=4898 fallbacks=16 sites=7 services=22
hl-native-detail: fills=16 site_collisions=0 shared_collisions=0 branch=140 syscall=0 fallback=3626 yield=884 completed=58122035 operand_callbacks=2780 operand_cache_hits=833 x86_public_exits=0 x86_public_syscalls=0 x86_syscall_vector_dirty=0
```

| Pair | Parent guest/wall | Candidate guest/wall |
|---:|---:|---:|
| 1 | 174287 us / 205 ms | 225443 us / 262 ms |
| 2 | 203011 us / 235 ms | 177993 us / 208 ms |
| 3 | 179819 us / 210 ms | 230632 us / 266 ms |
| 4 | 178573 us / 208 ms | 179696 us / 208 ms |
| 5 | 183240 us / 214 ms | 239317 us / 276 ms |

Parent guest time was min/median/max 174287/179819/203011 us with 6.10% CV;
candidate was 177993/225443/239317 us with 13.97% CV. Parent wall time was
205/210/235 ms with 5.58% CV; candidate was 208/262/276 ms with 13.63% CV.
Paired guest deltas were +51156, -25018, +50813, +1123, and +56077 us.

The pinned retained C engine was run directly on CPU 17 after one warmup:

```text
hl-engine-linux-aarch64 combined-bench-aarch64 --divisor 20 --phase memory
```

Its five guest samples were 6734, 6889, 6678, 6647, and 6805 us, all with
checksum `36526`; min/median/max were 6647/6734/6889 us and CV was 1.45%.

No performance claim is accepted. The Rust ranges overlap, one candidate pair
was faster, another was effectively equal, and candidate variance was high.
The median direction suggests a possible regression, not a demonstrated
improvement. The candidate was reverted; a future implementation requires a
lower-noise measurement that demonstrates benefit before acceptance.

### Rejected authenticated slot-zero fast path

A second, independent experiment started from parent
`66321702ad2278f1e13c2344e0f19ff4c8c7a398` and produced candidate
`f7b3d3bd32b3846733047943abbbcf0a67a493b0`. The retained C oracle remained
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. Typed instrumentation preceding
the experiment observed 43,460,082 selector decisions in three memory-phase
invocations: slot zero served 40,498,839 (93.186292%), the other three slots
served 2,954,352 (6.797852%), and only 6,891 (0.015856%) were cold misses.

The candidate appended an authenticated slot-zero projection to the CPU ABI.
The executor published it from the entry-authenticated read token and
incarnation only for a bounded, valid view-zero shape. Generated accesses
still checked overflow, lower and upper bounds, and read permission before
consuming the projected delta. Every rejected guard fell through to the
existing four-view selector. This shortened the common guard from about 32 to
20 executed words, but added 40 static words overall: the generated corpus
grew from 923 to 963 words, and both pair-read and scalar-read bodies grew by
20 words.

The warning-strict `aarch64_trace`, `aarch64_single`, `aarch64_cycle`, CPU
layout tests, and `git diff --check` passed on the candidate. The focused Rust
executor suite reported 60 passed, two ignored, and one pre-existing x86
strsearch differential failure (flags 5 versus 2053); no AArch64 candidate
test failed.

The exact A/B/C comparison used the same temporary memory-phase manifest in
all three trees: divisor 20, five measured repetitions, and checksum `36526`.
The harness also performed its normal cold invocation and one warmup. Runs
were sequential parent, candidate, then null control, without explicit CPU
affinity, using a development-profile `cargo run`:

```text
CARGO_BUILD_JOBS="$(nproc)" \
  CARGO_TARGET_DIR=/tmp/husklet-a64-slot0-target \
  cargo run -q -p testing --bin testing -- bench combined --isa arm64
```

The null control used the candidate's source and emitted fast-path code but
forced `slot0_valid` to zero. Every invocation reported identical native
diagnostics:

```text
hl-native: runs=900 builds=229 hits=4898 fallbacks=16 sites=7 services=22
hl-native-detail: fills=16 site_collisions=0 shared_collisions=0 branch=140 syscall=0 fallback=3626 yield=884 completed=58122035 operand_callbacks=2780 operand_cache_hits=833
```

| Tree | Wall cold | Wall min/median/p90/max | Guest min/median/p90/max |
|---|---:|---:|---:|
| parent | 1837 ms | 1796/1800/1803/1803 ms | 1575846/1577503/1581363/1581363 us |
| candidate | 1813 ms | 1789/1808/1892/1892 ms | 1566947/1583490/1658488/1658488 us |
| null control | 1821 ms | 1816/1852/1906/1906 ms | 1593583/1621808/1678209/1678209 us |

The candidate recovered most of the overhead exposed by disabling its
projection, but it did not beat the parent: guest median regressed by 0.38%
and wall median by 0.44%, while variance and static code size increased. The
retained-C result recorded in `PERFORMANCE.md` for the named memory/divisor-20
workload is 6839 us median, making these development-profile Rust medians about
230.66x and 231.54x retained C respectively; the gap did not improve. The
candidate was therefore rejected and its production branch and build artifacts
were deleted.

These approximately 1.58-second guest results must not be compared blindly
with the earlier 179819-us parent result in the preceding section. The earlier
experiment used release binaries, five alternating parent/candidate pairs,
one measured repetition per invocation, and CPU-17 affinity under the
performance governor. This experiment used development-profile binaries, five
samples within one invocation, sequential A/B/C ordering, and no explicit
affinity. Only the direction within each internally consistent experiment is
evidence; their absolute times are not interchangeable.

### Accepted targeted acquire

The exact parent for this experiment was
`d549362dc` and the retained read-only C oracle was
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The complete retained paths
reviewed again were `src/translator/guest_memory.c`, including
`hl_guest_memory_bind`, `hl_guest_memory_resolve_exec_span`, and the data-pin
entry points, and `src/linux_abi/thread.c`, including the GNA/GRO writer
locks, generation readers, `gna_hit`, `gna_prefix`, `gro_hit`,
`hl_linux_bus_fault`, file-map publication, and exec teardown. Their common
direct translated-access path does not execute a global read barrier for each
operand; generation-qualified readers acquire only the state whose
publication they consume.

In Husklet, `MemoryLedger` owns logical mappings and their generation.
`Coordinator::project_contiguous` retains checkpoint admission, the mapping
transaction lock, and projected host storage for the complete native call.
`ProjectionLease` owns additional views and either publishes successful dirty
ranges or rolls back reservations. In the native owner,
`src/executor.c::run_view_publish` writes every bounded view, count, and
incarnation before release-storing `cpu->read_token`. The CPU record has one
execution owner; fallback promotion republishes the payload before native
re-entry. Mapping mutation, fork repair, and teardown cannot retire the
storage through the retained projection lease.

Previously `read_cache` and `write_cache` loaded `read_token` normally and
then executed `DMB ISHLD`. The accepted sequence instead forms the token
address and uses `LDAR` on that exact release-published object. This preserves
the C11/AArch64 release/acquire edge that orders the immutable payload while
avoiding a global load barrier on every guarded access. Range, overflow,
permission, incarnation, fault, write-owner, dirty-publication, and
executable-write checks are unchanged. The change emits AArch64 code only;
x86-64 and host-specific mapping adapters are unchanged.

Both trees were warning-strict release builds. The candidate's focused native
suite passed 91 tests with no failures and two ignored tests. The guest source
hash was
`ec97f6f5c598f6fc229231dbf4751fb298ebaf1ae04c530d8aecbc7a1ec926af`;
the compiled guest hash was
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`.
Baseline and candidate engine hashes were respectively
`5af07d5edc3e22f6821dae70232a843d096046db459992a95edbdb5d594b97ec`
and
`1f2c8dc19568f518ffd62ae1fe57a57cb4c0ca9d29efc099e2546d23807e615e`.
One candidate runner drove both engines, eliminating runner differences.

Each row was a separate release-engine invocation pinned to CPU 17. Order
alternated by pair, and engine options were passed through the typed command
surface:

```text
taskset -c 17 testing benchmark run \
  --provider rust-engine --arch arm64 --binary combined_arm64 \
  --engine <baseline-or-candidate> --repeats 1 \
  --engine-option HL_NATIVE_EXECUTION=1 \
  --engine-option HL_NATIVE_DIAGNOSTICS=1 -- \
  --divisor <value> --phase <phase>
```

The required float-SIMD workload was neutral. Its seven baseline samples were
13,355,503, 13,703,441, 13,587,209, 13,637,522, 13,628,082, 13,665,672,
and 13,651,953 microseconds. Candidate samples were 13,656,664, 13,604,059,
13,620,877, 13,576,944, 13,639,683, 13,717,050, and 13,695,242
microseconds. Medians were 13,637,522 and 13,639,683 microseconds: a 0.016%
candidate difference within noise, with two of seven pair wins. Every row had
checksum `115136405225`.

The read-dominant memory control demonstrated the affected path. Baseline
samples were 31,927, 32,004, 31,893, 33,266, 31,582, 31,820, and 31,883
microseconds. Candidate samples were 31,492, 31,256, 31,532, 32,429, 31,264,
31,166, and 31,136 microseconds. Candidate won all seven pairs; medians moved
from 31,893 to 31,264 microseconds, a 1.972% improvement. Every row had
checksum `7190` and identical causal diagnostics: 5,962,557 full guards, 819
guard fallbacks, 645 operand callbacks, 174 operand-cache hits, 11,968,016
completed instructions, and matching run, build, hit, dirty, relocation, and
IBTC counters.

At final capture the host had 14 GiB available RAM, 17 GiB free swap, 94 GiB
free on the repository filesystem, 7.1 GiB free in `/tmp`, and load average
0.48/3.27/6.02. The targeted acquire is accepted: it materially improves the
read-heavy control without changing observable results, diagnostics, emitted
instruction count, or the required float-SIMD workload.
# Sparse publication-certificate oracle audit

The retained implementation was read in `../engine/src/translator/cache.c`
(`jit_cache_init`, `map_idx`, `map_host`, `map_body`, `map_put`,
`map_invalidate_source_ranges`, pending-link add/resolve/patch/reset, cache flush
and fork reset), `../engine/src/core/dispatch.c` (`run_guest`, cache lookup,
translation under `g_jit_lock`, flush and block entry), and
`../engine/src/translator/guest/aarch64/translate.c` (`translate_block`, source
fetch, emission, link registration and publication). The C cache is
process-global, generation-owned, serialized for translation by `g_jit_lock`,
published only after executable bytes and map metadata are complete, and reset
with pending/resolved link state on flush/fork. Its AArch64/x86 and host W^X
branches change encoding and publication mechanics, not guest identity.

Rust ownership maps those capabilities to `hl_native_cache` entries, arena,
bounded pending/resolved arrays, and the executor mutation gate. Sparse
certificates add an authenticated identity to that same lifetime: reserve is
invisible, validity is the final release publication after `ENTRY_LIVE`, and
cancel/invalidate/reset/fork revoke it. The 4096 record slots are monotonic and
never reused during an executor lifetime; validity revocation therefore cannot
race a stable-copy reader with record replacement or ABA. Exhaustion leaves
ordinary translation operational with identity zero. Relocation records capture
source and target certificate identities and fail closed on replacement. A
preserving fork first restores every resolved executable span to its cold image
under the arena W^X transition, publishes each restoration, removes inherited
IBTC ingress, and only then revokes certificate validity; a publication failure
poisons cache admission without discarding the unresolved metadata. Rollback
poisons the cache if either relocation or entry cleanup fails. Certificate reads
return a caller-owned copy from bounded immutable storage, never an internal
pointer.
