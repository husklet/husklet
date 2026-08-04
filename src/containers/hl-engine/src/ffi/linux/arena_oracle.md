# Linux arena unmap oracle

Audit date: 2026-08-04.

## Retained implementation studied

- `../engine/src/linux_abi/syscall/mem.c`, `svc_mem`, syscall 215: validates a
  nonzero, guest-page-aligned, non-wrapping range, then treats an unmapped or
  partially mapped range as a successful `munmap` and splits every intersecting
  registry entry.
- `../engine/src/linux_abi/container/vfs/gmap.c`,
  `hl_gmap_unmap_range`, `hl_gmap_split_range`, and
  `hl_exec_mapping_discard_range`: remove only mapped intersections, preserve
  surviving heads and tails, and retire native ownership for released spans.
- `../engine/src/linux_abi/syscall/mem.c`, `anon_split_unmap`, and
  `../engine/src/linux_abi/thread.c`, `futex_shared_unmap`, `filemap_unmap`,
  `gna_clear`, and `gna_add`: update each
  auxiliary registry against the actual released intersection while preserving
  tail offsets and identities.
- `../engine/src/host/linux/host.c`, `hl_linux_memory_unmap_range`: Linux
  `munmap` owns the native split; partial release records holes without consuming
  the remaining mapping handle. The host call holds the host registry lock.

The retained syscall runs the mapping transition under its stop-the-world and
registry locks, drops mapping-associated state only after successful native
release, and leaves already absent pages harmless. `mprotect` is different: its
source range must be completely mapped and a hole yields `ENOMEM`. Both guest
architectures share this syscall implementation; host-page granularity changes
physical release details but not guest-visible hole semantics.

## Rust ownership mapping

- `hl-memory::Coordinator` serializes the transaction and owns the published
  guest ledger and backing pins.
- `hl-runtime` validates Linux syscall geometry and maps memory failures to
  Linux errno.
- `ffi/linux/arena.rs::Ledger` is the concrete Linux host projection shadow.
  Unmap now clips work and rollback protection to the mapped intersections;
  protect continues to require complete coverage.
- `ffi/linux/virtual/memory.rs::Memory` owns the reservation, native protection
  changes, staged-operation ordering, and reverse-order compensation. The
  published ledger is changed only after every native operation succeeds, so a
  failed batch retains exact backing identities, offsets, protections, and hole
  geometry while compensation restores access only for intersections hidden by
  an earlier unmap.

No retained-C source was modified.
