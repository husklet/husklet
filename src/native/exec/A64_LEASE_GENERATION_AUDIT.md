# AArch64 projection-lease generation audit

## Scope and exact trees

This bounded implementation-prerequisite audit used Husklet baseline
`cf15cdd33`, retained read-only oracle `/Users/x/dd/engine`, certificate
candidate `agent/a64-ingress-cert` at `a21de7906`, and dirty-publication
candidate `agent/arm-dirty-coalesce` at `ad2a377c0`.  The retained tree was
not modified.  No production change is made because the independently
comparable value has two owners which cannot be split without creating a
false lifetime proof: Rust owns the live projection, while native execution
owns fork repair and every translated ingress.

## Retained C and assembly lifecycle

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

## Rust/native ownership matrix

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

## Candidate interaction and blocker

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
