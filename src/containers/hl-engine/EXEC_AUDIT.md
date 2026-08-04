# Executable memory oracle audit

This audit covers executable-byte reads and the mapping authority that guards
them. The retained C tree at `../engine` was read-only throughout the audit.

## Retained implementation studied

- `src/translator/guest_fetch.c`: `hl_guest_fetch_exec`, `fetch_walk`,
  `chunk_valid`, `span_for`, and `span_hit` validate every instruction-fetch
  chunk before copying it. Cross-page reads stage at most 64 bytes so a later
  fault cannot publish a partial instruction buffer.
- `src/translator/guest_memory.c` and `guest_memory.h`:
  `hl_guest_memory_resolve_exec` and
  `hl_guest_memory_resolve_exec_span` retain executable authority as a distinct
  operation instead of converting it to ordinary read authority.
- `src/linux_abi/logical_vma.c` and `logical_vma.h`:
  `hl_logical_vma_resolve`, `hl_logical_vma_resolve_exec`, and
  `hl_logical_vma_resolve_exec_span` require `HL_LOGICAL_VMA_EXEC`, return
  `EACCES` when it is absent, reject overflowing or cross-view ranges, and use
  acquire loads of immutable mapping snapshots. Map, unmap, protect, reset, and
  transaction publication advance the generation. Mutation and retired-view
  reclamation use the ledger mutex; executable lookup itself is lock-free.
- `src/core/target/aarch64.c` and `src/core/target/x86_64.c`:
  `engine_global_init` binds the guest-memory operations and direct-range
  validator. The x86 target also binds the decoder fetch seam. Both targets use
  the same executable authority; neither substitutes readable authority.
- `src/linux_abi/thread.c`: the exec address-space reset path calls
  `hl_logical_vma_global_reset_quiescent`; the process-owned backing mechanisms
  survive that logical mapping reset and remain available while the new image
  is loaded.

The retained lifetime model is an immutable published VMA snapshot plus a
monotonic generation. A cached span is valid only while its generation matches.
Backing windows outlive readers until quiescent reclamation. Failed resolution
copies no bytes. There is no blocking, cancellation, or partial-result contract
for instruction fetch: success copies the complete requested range and failure
is atomic. Architecture-specific code differs only in decoder binding and
instruction width; host-specific direct mapping validation remains behind the
bound target operation.

## Capability mapping

| Retained capability | Rust owner | Status |
|---|---|---|
| Guest mapping ledger and exact protection resolution | `hl-memory::MappingCoordinator` | Implemented |
| Complete-range, cross-region executable fetch | `MappingCoordinator::read_spans` and `ArenaMemory::fetch` | Implemented |
| Failure-atomic staged cross-region fetch | `MappingCoordinator::read_spans` | Implemented |
| Host mapping protection revalidation | `VirtualMemory::snapshot_read` | Implemented |
| Preserve the caller's executable authority through host revalidation | `MemoryAccessHost::read` and `MappingHostAdapter::read` | Fixed by the change accompanying this audit |
| Executable mapping publication and generation tracking | `hl-memory` executable versions and mapping transition observer | Implemented |
| Snapshot/fork copy of execute-only regions | `AddressSpace::copy_segment` with `frozen_snapshot_read` | Implemented |
| Exec replacement retains the address-space backing mechanism | `AddressSpace::exec_space` with `VirtualMemory::with_inherited_backings` | Implemented |

Before this change, `MappingCoordinator::read` correctly admitted
`Protection::EXECUTE`, but the host port discarded that typed authority and
`MappingHostAdapter` rechecked the range as `Protection::READ`. Execute-only
anonymous, file-backed, shared, and restored mappings therefore failed after a
successful coordinator check. The port now carries the admitted protection to
the host. Write reconciliation similarly carries write authority, so the repair
does not weaken a mapping or imply that execute/write permission grants read
permission.

The same cohort exposed a separate construction divergence: `exec_space`
required a native shared-backing registry even when the source arena correctly
owned the bounded snapshot-backing adapter. Inheriting the source arena's
backing mechanism matches fork construction and the retained exec lifecycle;
it does not retain old mappings, bytes, or mapping authority in the empty exec
space.
