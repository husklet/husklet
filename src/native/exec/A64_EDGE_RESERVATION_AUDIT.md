# AArch64 cross-entry reservation blocker

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

The prerequisite metadata is complete but deliberately unused:

| Capability | Rust state | Result |
|---|---|---|
| Post-admission target identity | `cache_entry.admitted_offset` | implemented |
| Exact destination charge | `resolved_relocation.relocation.target_instruction_count` | implemented |
| Cold-edge reversibility | `expected` plus resolved-to-pending invalidation | implemented for one word |
| Edge-local admission program | no reserved patch span or typed description | missing |
| Atomic multiword patch/restore | relocation publishes one four-byte word | missing |

`hl_a64_stub_budget_begin` orders interrupt, asynchronous-token, and budget
checks before subtracting the complete block charge. Its `admitted_offset`
follows all of those checks. Branching there is correct only after the edge has
performed the same admission transaction. Subtracting the recorded target
count without first checking the budget can underflow; checking only the budget
can delay an already-visible interrupt or token indefinitely across an acyclic
chain; branching to `body_offset` merely repeats the existing full guard and
does not consume either new metadata field.

Every direct and conditional relocation currently owns exactly one patchable
instruction. `patch()` validates and replaces that word with `b body_offset`,
publishes four bytes, and invalidation restores the same word. There is no safe
indivisible one-word encoding for the required interrupt load/branch, optional
token acquire/load/branch, budget compare/branch/subtract/store, and branch to
`admitted_offset`. Adding a NOP reservation only in the emitters would expose a
partially described span to current relocation and invalidation code; changing
only relocation would overwrite ordinary cold-exit instructions. Either is an
unsafe partial protocol.

## Required bounded mechanism

The next implementation must be one atomic edge-reservation protocol spanning
emission and cache ownership: a typed fixed-size patch span with its complete
cold image, target admission metadata and reach checks, construction of the
full interrupt/token/budget sequence off-line, one publication operation for
the complete span, and exact full-span restoration before invalidation or
retirement. Conditional fall and taken exits, cold target resolution, wildcard
epoch rebinding, capacity rejection, cycle-retained edges, and fork/reset
teardown must all use the same representation. Tests must prove zero guest
progress on insufficient budget, interrupt and asynchronous-token precedence,
exact target charging, both conditional successors, cold-to-hot resolution,
target replacement with a different instruction count, invalidation restore,
and a bounded cycle retaining progress.

That protocol is larger than the edge-emitter-only slice assigned to this
lane. No production or test source was changed, and no compilation artifacts
were created.
