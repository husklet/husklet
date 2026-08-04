# Native hot-path parity audit

This source audit ranks generic native-execution gaps against the retained C
engine.  It changes no production behavior.  Husklet was inspected at
`7e368ec300ad6c3a136ce1b0b2ec052a34fc6306`; the read-only retained oracle was
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`.  The retained checkout has an
unrelated deleted packaging README and untracked `.claude/`, so that revision
identifies the source studied rather than claiming a clean checkout.

## Oracle and Rust paths studied

The complete retained AArch64 memory/dispatch path inspected was:

- `../engine/src/translator/guest/aarch64/translate.c`:
  `emit_a64_soft_guard_begin`, `emit_a64_soft_guard_end`,
  `emit_a64_soft_exit_site`, `emit_fold_mem`, `aarch64_soft_tlb_miss`,
  `aarch64_soft_tlb_span`, `aarch64_soft_prepare_bounce`, and
  `aarch64_soft_bounce_commit`;
- `../engine/src/translator/guest/aarch64/dispatch.h`: `R_SOFTTLB`,
  `R_SOFTSPAN`, and `R_SOFTCOMMIT` dispatch ordering;
- `../engine/src/translator/guest/aarch64/stubs.c`: `emit_spill_gpr`,
  `emit_spill`, `emit_exit_const`, `emit_ibranch_steal`, and
  `emit_chain_exit_from`;
- `../engine/src/translator/guest/aarch64/cpu.h`: the CPU-owned soft-TLB,
  cross-page bounce, miss, vector-dirty, and chain state; and
- `../engine/src/translator/cache.c`: `stw_register`, `stw_unregister`,
  `stw_before_translated`, `stw_after_translated`, `stw_mapping_begin`, and
  `stw_mapping_end`.

The corresponding Rust-owned implementation inspected was:

- `src/native/exec/src/arch/aarch64/guard.c`: `read_cache`, `write_cache`,
  `legacy_begin`, `hl_a64_guard_write_begin`, and `hl_a64_guard_written`;
- `src/native/exec/src/arch/aarch64/projection.c`:
  `hl_a64_projection_resolve` and `flush_dirty`;
- `src/native/exec/src/arch/aarch64/pair.c`, `single.c`, `structure.c`,
  `ordered.c`, and `zero.c`: guarded native memory operation families;
- `src/native/exec/src/arch/aarch64/stub.c`: full prologue, spill, and public
  exit ownership;
- `src/native/exec/src/arch/aarch64/direct.c`: direct-branch exit ownership;
- `src/native/exec/src/executor.c`: `run_aarch64`, run-view publication,
  execution admission, resolver exit/re-entry, and epoch handling;
- `src/native/cpu/layout.tsv`: CPU ABI state for views, dirty ranges, active
  authority, and the dormant certificate seam;
- `src/containers/hl-engine/src/native/executor.rs`: live projection lease,
  native request construction, dirty-record validation, and publication; and
- `src/runtime/hl-memory/src/mapping/projection.rs`: generation-qualified
  projection ownership, mapping/checkpoint admission, rollback, exact dirty
  reconciliation, executable invalidation, and exclusive invalidation.

## Ownership, lifetime, and ordering

Retained C gives each registered CPU one cached inclusive-first/exclusive-last
interval, delta, and protection tuple.  The lock-free generated hit path is
safe because mapping mutation takes the global JIT lock and thread-registry
lock, parks translated peers, clears every registered CPU tuple before backing
retirement, publishes the new logical snapshot and rejection hull, then opens
the gate.  Registration and teardown add/remove the CPU pointer under the
registry lock.  A miss preserves exact PC, width, direction, and effective
address, then enters one shared cold resolver.  Discontinuous cross-page
stores validate both sides, block signals, write through a bounded bounce, and
publish only after the copy succeeds.

Husklet deliberately uses stronger local ownership.  A `ProjectionLease`
holds checkpoint admission, the mapping transaction mutex, host projection
storage, write reservations, and generation identity for the complete native
run.  `run_aarch64` authenticates mapping incarnation and direct authority,
publishes at most four generation-qualified views with release/acquire
ordering, and clears transient authority on exit.  A successful store records
its exact guest interval only after the host instruction.  Rust validates those
records before reconciling non-coherent shared backing, invalidating executable
translations and exclusives, and committing or rolling back reservations.
Overflow degrades to full-view publication; a fault before the store publishes
nothing.  This is correct but puts the authority lookup and dirty-journal state
machine in the generated path of every ordinary access.

The retained implementation has POSIX-specific stop-the-world signaling and a
macOS exception in its direct host-range probe; its emitted AArch64 hit path is
otherwise host neutral.  Husklet's generated AArch64 path is compiled only on
AArch64 hosts.  x86-64 guests on an AArch64 host use the separate x86 frontend,
projection journal, and run loop; neither implementation makes the proposed
AArch64 certificate transferable to that ISA without its own audit.

## Ranked capability matrix

The rank uses the exact measurements already recorded in `PERFORMANCE.md`, not
code size alone.  The pinned AArch64 memory phase is about 370 times slower
than retained C and executes 17,150,872 guards, while only 654 accesses
(0.003813%) call the Rust resolver.  Vector-pair loads/stores account for
99.9935% of the guarded accesses.

| Rank | Capability | Retained C | Husklet | Evidence and decision |
| ---: | --- | --- | --- | --- |
| 1 | Projected-view hit | One CPU-owned interval/protection/delta tuple; flag-free hit; one shared cold resolver | Token/incarnation/count validation plus an up-to-four-view linear selector at every access | **Largest measured gap.** A first-view pair read executes about 34 instructions; an empty-journal pair store about 63. Resolver work is too rare to explain the gap. |
| 2 | Successful-store publication | Aggregated SMC/bounce commit, with no projection journal on ordinary identity writes | Capacity decision before every store and exact range/owner merge after every success | **Co-dominant emitted cost, stronger Rust contract.** Must preserve no-publication-on-fault, exact executable ranges, exclusives, reconciliation, and rollback. A blanket journal removal is invalid. |
| 3 | Direct and indirect chaining | Resolved edges enter target bodies with live guest state; misses share spill/dispatch; SMC changes chain policy | `direct.c` emits a full spill and `HL_NATIVE_EXIT_BRANCH`; dispatcher re-enters through a full prologue | **Missing generic mechanism**, but the measured memory row sees only 70 branch boundaries per 34 million instructions, so it is not the first memory-parity lane. |
| 4 | Vector-clean syscall spill | Runtime `vdirty` currency lets a clean syscall omit 512 bytes of vector publication; all asynchronous and dirty exits remain full | Every public exit stores the complete 752-byte guest state and re-entry reloads it | **Bounded secondary gap.** Existing event-mix analysis caps the observed traffic reduction near 6.12%; add typed exit counters before implementation. |
| 5 | Cross-page discontinuity | Cold resolver owns bounded bounce, signal masking, retry, partial/fault ordering, and post-success commit | Projection resolver requires a constant host delta across the complete access and otherwise returns to Rust/fault handling | **Semantic mechanism missing from a persistent cache.** It must be part of rank 1's cold path, not patched into hot emitters. |
| 6 | Mutation/fork retirement of cached backing | Registry pins CPU identity; stop-the-world clears tuples before reclamation and refreshes after publication | The run lease pins backing, but the dormant `certificate_valid/delta` carries no page, authority, incarnation, or fork identity and is cleared at entry | **Prerequisite missing.** Persisting the dormant bit would permit stale-delta or cross-page host access. |

Translation publication, cache lookup, Rust resolver callbacks, and dispatcher
service are not the current primary lane: the pinned row records only 113
builds, 1,426 block-cache hits, 654 callbacks, and 19 ordinary fallbacks over
34,049,536 completed guest instructions.  Adding opcode families or more than
four run views likewise preserves the dominant per-access selector and journal
work.

## Next implementation lane

Implement one generation-qualified, authority-bound **last-view certificate**
for AArch64 memory accesses.  It is a single coherent ownership mechanism, not
an emitter shortcut:

1. Extend the CPU ABI with an indivisible record containing guest first/last,
   host delta, permissions, mapping incarnation, direct-authority identity,
   and a lease generation.  The live `ProjectionLease`, not the CPU record,
   continues to own storage.
2. Publish/authenticate that record at native-run entry and every direct-chain
   ingress.  A hit performs only checked end construction, interval and
   permission tests, then applies the delta without scanning the four-view
   table.  Any mismatch uses one shared bounded resolver.
3. Rotate/clear its generation before mapping replacement, authority-token
   retirement, fork-child repair, executor destruction, or any transition that
   can reclaim projected backing.  Do not copy retained C's process-global
   registry; bind invalidation to Husklet's instance-owned execution gate and
   projection lease.
4. Preserve exact site provenance.  Stores still reserve capacity before the
   host instruction, publish only after success, and force an epoch exit for
   executable aliases.  The cold path must handle adjacent discontinuous views
   without claiming an unwritten span.
5. Add fail-first stale-incarnation, rotated-authority, cross-page,
   permission, faulting-store, executable-write, mutation-race, fork, teardown,
   and direct-chain tests.  Typed counters must distinguish certificate hit,
   cold miss, stale rejection, cross-span, and access form.

Only after the certificate reduces dynamic guard work should a second audit
classify write views whose host backing is coherent and non-executable.  The
current `ProjectionLease` still uses exact ranges for exclusive invalidation
and reservation commit even when shared reconciliation is unnecessary, so a
coarse/exact publication mode cannot safely be inferred from protection bits
or `shared_backing_is_coherent()` alone.

Acceptance requires an exact committed tree, native diagnostics selected via
the typed engine options, identical checksum and exit/counter mix, and at least
five warm measurements alternating baseline and candidate on a pinned CPU.
The retained C binary/source binding must be rebuilt or reported as an
artifact-bound control.  No performance or parity claim follows from this
audit alone.
