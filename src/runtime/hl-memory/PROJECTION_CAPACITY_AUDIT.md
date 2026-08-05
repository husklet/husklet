# Projection capacity and dirty-owner audit

This audit was performed on 2026-08-04 before changing the live projection
bound. The retained C tree at `/Users/x/dd/engine` was read-only.

## Retained C ownership

The complete relevant retained domain was inspected in
`src/linux_abi/logical_vma.c` and `.h`, `src/translator/guest_memory.c` and
`.h`, `src/translator/cache.c`, and `src/linux_abi/checkpoint.c`.

`hl_logical_vma_ledger` owns mutable entries, canonical shared backing
references, immutable reader snapshots, retired snapshots, and a monotonic
generation. Mapping preparation allocates and maps a staged ledger while the
live ledger lock is held. Commit is allocation-free: it replaces entries,
publishes a sorted immutable snapshot, then advances the generation. Abort and
every allocation, descriptor, or host-map failure unwind retained backing
references. Unmap and protection split entries under the same lock. Snapshot
reclamation happens only at a stop-the-world quiescent boundary, so lock-free
resolution cannot retain freed storage.

`hl_guest_memory_pin_data` validates nonempty, non-overflowing ranges before
delegation and returns a lifetime token released by `unpin`. The retained JIT
does not keep a bounded ordinary-write journal: direct mappings make normal
stores host-coherent. Executable changes instead retain bounded CPU-local SMC
ranges until dispatcher synchronization; saturation is conservative and never
allows an untracked mutation. `ckpt_dump_self` stops guest threads before CPU,
mapping, and sparse page capture, and failure ends the stop interval without
publishing a complete checkpoint. Architecture-specific branches change CPU
image format and fault reconstruction, not mapping identity.

## Rust mapping and invariant

`ProjectionLease` owns checkpoint admission, the mapping transaction guard,
host projection lifetimes, stable backing identity, write reservations, and a
generation-qualified view table. Mapping or checkpoint mutation therefore
cannot revoke a view during its synchronous native use. Drop rolls back every
uncommitted reservation. Exact dirty publication validates each range against
its owning writable view before reconciliation, exclusive invalidation, and
reservation commit.

Native writers must archive a nonempty dirty interval with its exact
`[view_first, view_last)` owner before changing active view metadata. Journal
capacity is checked before a store that would require another record; a
faulting store publishes nothing, while a completed store is recorded before
any epoch or interrupt exit. Saturation falls back to conservative full-view
publication rather than losing a completed write.

## Capacity correction

`LIVE_PROJECTION_MAXIMUM` names the complete lease bound, but
`project_additional` previously compared it only with `additional.len()`.
Together with the primary view this admitted 65 views under a stated maximum
of 64. The corrected admission check uses `projection_count()`, keeping storage,
write reservations, stable `u16` indices, and teardown work within the public
bound. Existing-view reuse is checked before capacity admission, so a full
lease can still resolve an already retained view without allocating or changing
ownership.
