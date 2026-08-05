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
