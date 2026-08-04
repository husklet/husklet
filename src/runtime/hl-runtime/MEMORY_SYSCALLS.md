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
