# Dirty publication allocation audit

## Retained C oracle

The retained tree was studied read-only at commit-independent path
`../engine` (resolved as `/Users/x/dd/engine`). The relevant owners are:

- `src/translator/guest_memory.c`: `hl_guest_memory_bind`,
  `hl_guest_memory_pin_data`, and `hl_guest_memory_unpin_data` own the coarse
  projection callback boundary. The bound operations table and generation
  pointer have process lifetime; pins have explicit caller lifetime. There is
  no per-write lock or allocation here, and invalid/overflowing requests fail
  before invoking the host adapter.
- `src/linux_abi/logical_vma.c`: `hl_logical_vma_map_shared` owns canonical
  shared-backing identity and immutable-snapshot publication. Mapping changes
  hold `ledger->lock`, allocate replacement state before publication, preserve
  retained file references, and unwind mappings/snapshots on every allocation
  or host-map failure. Host-page alignment is explicit, including larger host
  pages; invalid input reports `EINVAL` and size exhaustion reports `ENOMEM`.
- `src/linux_abi/checkpoint.c`: `ckpt_dump_self`, `ckpt_dump_pages`, and
  `ckpt_dump_region_bytes` stop all guest threads at a block boundary, copy CPU
  images while stopped, enumerate mapping/logical-VMA state, and sparse-dump
  nonzero pages. `g_filemap_lock` protects file-map metadata. Failure ends the
  stop-the-world interval and does not publish a complete image. Logical VMA
  pages use copy-out rather than exposing host placement. The architecture
  branch selects the CPU image format; logical mappings and host mapped-range
  checks cover indirect and native host layouts without changing guest VAs.

The Rust mapping is `ProjectionLease` in
`src/runtime/hl-memory/src/mapping/projection.rs`. It retains checkpoint
admission, the mapping transaction mutex, host projection lifetime, backing
identity, generation, and pre-write reservations. `publish_written_ranges`
validates every exact range against a retained writable view, reconciles
noncoherent shared backing, publishes executable aliases, invalidates
exclusives, and commits only after successful native writes. Drop rolls back
uncommitted reservations. `CheckpointActivity` and
`mapping/checkpoint.rs::{freeze_checkpoint,with_frozen_snapshot}` map the C
stop-the-world ownership to Rust admission/freeze ordering. No C dirty-range
journal exists to port directly; exact range publication is a stricter Rust
capability required by the native adapter.

## Capability comparison

| Capability | Retained C | Rust |
|---|---|---|
| Stable projection lifetime | pin/unpin callback token | `ProjectionLease` host projection |
| Mapping/checkpoint exclusion | global STW/mapping locks | activity admission plus transaction mutex |
| Shared backing identity | canonical vnode extent | typed `Backing` plus reconciliation |
| Successful-write publication | coarse host mapping state | bounded exact dirty ranges and reservations |
| Failure rollback | map/snapshot unwind | reservation rollback in error paths and `Drop` |
| Guest address independence | logical VMA copy in/out | integer storage address scoped to lease |

## Hot-path change and measurement

Every exact native-write exit copied all retained `ProjectionView` values into
a temporary `Vec` solely to classify at most 64 dirty ranges. The lease already
owns the same bounded views. Classification now checks the primary and retained
additional views directly, removing one heap allocation without changing
publication order, validation, or rollback.

An optimized allocation/classification microbenchmark used four retained views,
four dirty ranges, and 5,000,000 calls per variant. Five paired repetitions in
the same executable produced medians of 220.45 ms before and 188.19 ms after
(14.63% lower). One noisy repetition favored the old variant, so this is
focused mechanism evidence rather than an end-to-end engine claim. The
structural result is deterministic: one view-table allocation and copy is gone
from each call.
