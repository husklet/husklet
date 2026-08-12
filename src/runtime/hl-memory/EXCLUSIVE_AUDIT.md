# Exclusive reservation lifetime audit

> **Historical replacement ownership:** Rows naming `hl-execution` document the
> deleted Rust executor. The selected C engine currently owns guest reservation
> execution; `hl-memory` remains a Rust control-plane service.

This audit covers the address-space lifetime of AArch64 local exclusive
reservations. The retained C engine at `../engine` was read-only throughout.

## Retained implementation studied

- `src/linux_abi/elf.c`: `nonpie_fixup` and `nonpie_sc` own the retained
  software LL/SC fallback. `g_llsc` is thread-local and records the translated
  host address, observed value, and width. A store-exclusive consumes the
  reservation before its atomic compare-and-swap, whether it succeeds or
  fails. Ordinary conflicting stores served by this fallback invalidate the
  monitor. High-address guest atomics use the host AArch64 monitor directly.
- `src/linux_abi/syscall/proc.c`: `fork_child_hooks` repairs process-private
  state after host `fork`; the committed `execve` path stops peer threads,
  resets the bound mapping and logical VMA registries, and loads a new image.
- `src/linux_abi/thread.c`: `thread_after_fork` repairs locks and task identity;
  `gna_reset` and the exec teardown reset old-image mapping state.
- `src/translator/guest/aarch64/cpu.h`: `G_SOFT_STATE_RESET` clears transient
  memory-operation state for fork and clone boundaries.

The retained software monitor has no explicit address-space generation field.
Its host address normally changes or becomes unmapped when exec replaces an
image, but reuse of the same host address and value is not a formal invalidation
mechanism. Rust therefore must preserve the Linux/AArch64 lifetime contract
directly instead of copying that incidental retained representation.

## Ownership and capability matrix

| Capability | Rust owner | Status |
|---|---|---|
| Per-task architectural local monitor | `hl-execution::Aarch64CpuState` | Implemented |
| Consume a monitor on every STXR attempt | `hl-execution::aarch64::atomic` | Implemented |
| Per-granule conflicting-write epochs | `hl-memory::ReservationEpochs` | Implemented |
| Shared-backing alias invalidation | `hl-memory::ReservationCoordinate` and `SharedObjectStore` | Implemented |
| Mapping mutation invalidation | `hl-memory::MappingCoordinator` | Implemented |
| Fork parent and child clear the monitor | `ExecutionSnapshot::fork_parent`, `Runtime::child_cpu` | Implemented |
| Checkpoint clears non-durable monitor state | `ExecutionCheckpointParticipant::snapshot` | Implemented |
| Exec starts from a fresh CPU image | `ffi::linux::execution::exec_image::CpuImage` | Implemented |
| Dynamic address-space replacement rejects an old monitor | `ffi::linux::execution::atomic_memory` | Fixed with this audit |

Before this change the engine adapter exposed the mapping ledger generation as
the execution reservation generation. A dynamically selected `ArenaMemory`
could load through one `SpaceImage`, then commit through its replacement. Both
coordinators begin with the same ledger generation and private write epoch, so
the old reservation could authorize a write into the new image.

The adapter now captures one `SpaceLease` for each exclusive operation and uses
its address-space incarnation as the execution reservation generation. Store
rejects a different incarnation before consulting the current coordinator.
Within the same incarnation, the coordinator's current mapping generation and
the captured per-granule write epoch retain mapping and conflicting-write
validation. A `SliceMemory` operation remains bound to its guarded lease, while
dynamic `ArenaMemory` observes replacement and rejects the stale monitor.

Atomic operations are serialized by the coordinator transaction lock. They do
not block on guest state, return partial results, or expose cancellation. The
same lifetime rule applies to scalar and pair reservations and is independent
of host architecture.
