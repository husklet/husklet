# AArch64 cross-entry reservation ownership

## Retained oracle

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

## Rust owner and comparison

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

## Delivered bounded mechanism

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
## Reservation consumer

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
