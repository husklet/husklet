# Runtime memory syscall composition

`RuntimeMemorySyscalls` joins Linux ABI plans, the guest mapping ledger, the
transactional mapping host, descriptor identities, and optional host memory
services. Guest geometry is always Linux 4 KiB geometry; host mapping adapters
remain responsible for safely projecting it onto a different host granule.

Implemented paths are `brk`, `mmap`/`mmap2`, `munmap`, `mprotect`, `mremap`,
`madvise`, `mincore`, `mlock`, `munlock`, `mlockall`, `munlockall`, `msync`,
and `memfd_create`. Mapping publication follows `MappingCoordinator` commit:
host staging failure or commit failure leaves the ledger unchanged. `mincore`
residency is staged and guest copyout must finish before success is returned.

File mapping requires an injected `DescriptorMappingSource`. Memfd objects use
`SharedObjectStore`, retain Linux OFD identity across descriptor aliases, and
remain alive through mapping pins after final descriptor close. The shared
`MemfdRegistry` must also be supplied to `RuntimeFilesystemSyscalls` for
`ftruncate`, `F_ADD_SEALS`, and `F_GET_SEALS`.

Host-dependent advice, residency, locking, synchronization, and break policy
require `RuntimeMemoryHost`; absent capabilities return `ENOSYS`. Huge-page
memfd creation is validated but returns `ENOSYS`. Mapping a memfd range beyond
its current size also returns `ENOSYS`: Linux permits the mapping and raises
`SIGBUS` only when an out-of-file page is accessed, which requires a future
fault-policy key in the mapping host rather than a misleading eager errno.

## Retained protection oracle audit

The 2026-08-04 protection audit used the retired C engine as a read-only oracle.
The complete syscall owner inspected was `../engine/src/linux_abi/syscall/mem.c`,
entry `svc_mem`, including `mmap` case 222, `mprotect` case 226, and the adjacent
`munmap`, `mremap`, `msync`, `mincore`, and advice transitions that mutate or
consume the same mapping state. Supporting ownership and access paths inspected
were:

- `../engine/src/linux_abi/syscall/helpers.c`: `anon_track`,
  `anon_update_prot`, and the private-anonymous mapping registry;
- `../engine/src/linux_abi/thread.c`: the GNA, GRO, and GBUS interval registries,
  their writer locks and fork repair, and `host_range_mapped`;
- `../engine/src/linux_abi/logical_vma.c`: shared logical-view pinning and the
  prepare/commit/abort protection transaction;
- `../engine/src/linux_abi/host_mman.h`: protection-bit translation and the host
  service seam;
- `../engine/src/host/linux/host.c`, `../engine/src/host/macos/host.c`, and
  `../engine/src/host/windows/memory.c`: anonymous/file mapping,
  address-keyed protection, and unmapping adapters;
- `../engine/src/translator/guest/aarch64/dispatch.h` and
  `../engine/src/translator/guest/x86_64/translit/translit.c`: executable-page
  publication and self-modifying-code invalidation. No assembly routine owns
  the Linux protection contract; assembly only reaches the architecture's
  syscall dispatcher.

The retained state is process/address-space scoped. Mapping, anonymous backing,
logical aliases, inaccessible ranges, read-only ranges, and bus-fault tails have
separate identities and lifetimes. A protection change validates the entire
range first, prepares every affected logical alias while holding the mapping
transition lock, performs fallible host work, then commits permission registries;
failure aborts the prepared logical transaction without publishing the requested
permissions. Fork handlers repair registry locks, and exec/unmap retire the
corresponding state. Linux hosts apply physical protection directly (guest
execute remains host-readable data for DBT); macOS widens writable transitions
to its host-page granule while retaining 4 KiB guest permissions logically;
Windows projects the same address-keyed transaction over `VirtualProtect`.

Linux accepts every combination of `PROT_READ`, `PROT_WRITE`, and `PROT_EXEC`,
including write-plus-execute. `svc_mem` rejects only unknown protection bits
(apart from `PROT_GROWSDOWN` and `PROT_GROWSUP`), misaligned starts, overflow,
or ranges containing a hole. A zero-length request succeeds without changing
state. Adding execute arms translation invalidation; adding write drops stale
translations before the store can become silent. The Rust owners preserve that
split: `hl-linux::MemoryAbi::mprotect` validates and constructs the request,
`RuntimeMemorySyscalls::range_operation` supplies Linux errno conversion and
coordinates the transaction, and `hl-memory::MappingCoordinator` owns mapping
identity, splitting, host staging, commit, and rollback.
