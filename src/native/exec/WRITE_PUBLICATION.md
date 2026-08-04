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
