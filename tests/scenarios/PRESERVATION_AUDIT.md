# Legacy scenario preservation closure

This audit records the deletion gate used while reconciling the final legacy
declarative fixtures at `747c2b3d0` with each category's `test.yaml`.

## Old-only declarative contracts

All 19 formerly old-only stable IDs now have folder-owned definitions with the
same ID, image, action semantics, targets, expected-failure metadata, timeout,
and output oracle.

## Rust group-only behavior

The former database cleanup mapping remains documented in its category oracle.
The language uniqueness and weird expected-failure invariants now have focused
unit tests in the repository testing application. Per-case scheduling owns
isolated container state, replacing the legacy category wrappers.

No API-behavior module was deleted by the declarative closure batches.

The per-category `ORACLE.md` files retain the detailed commands, readiness and
entrypoint contracts, scheduler differences, and existing owner mappings.
