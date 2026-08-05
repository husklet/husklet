# AArch64 cross-entry metadata audit

## Retained oracle

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

## Rust owner and capability matrix

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
