# Memory compatibility oracle audit

Retained C was studied read-only in `../engine/src/linux_abi/syscall/mem.c`, `../engine/src/linux_abi/host_mman.h`, `../engine/src/linux_abi/host_uio.h`, `../engine/src/linux_abi/linux_abi.c` vector entry points, `../engine/src/linux_abi/sentry.c` iovec admission/copyback, and the mapping/pipe/fork paths. Mapping and descriptor identities own state; whole vectors are admitted before payload transfer, Linux caps and partial results are preserved, pipe writes within `PIPE_BUF` remain atomic, blocking operations wake or interrupt with exact errno, and last-close/fork teardown is explicit. Rust ownership maps to `hl-memory`, `hl-descriptor`, and `hl-runtime` memory/filesystem/pipe composition.

The four cases preserve lock scope, vector caps, validation ordering, atomic pipe writes, scatter reads, and forked aliases on both ISAs.

## Complete retained memory domain

The complete compatibility migration also studied the retained implementation,
not only its fixtures.  The primary entry point is `svc_mem` in
`../engine/src/linux_abi/syscall/mem.c`; its syscall cases cover `brk`,
`munmap`, `mremap`, `mmap`, `mprotect`, `msync`, `mlock`, `munlock`,
`madvise`, `mincore`, `mlockall`, and `munlockall`.  Supporting ownership and
execution paths were inspected in:

- `../engine/src/linux_abi/logical_vma.c` and `logical_vma.h` for canonical
  file-backed storage, guest views, reference-counted backing lifetime,
  snapshot publication, pins, and fork cloning;
- `../engine/src/linux_abi/page.c` and `page.h` for guest-page versus host-map
  granularity and range validation;
- `../engine/src/translator/guest_memory.c`, `guest_memory.h`, and
  `guest_fetch.c` for bounded read/write pins, executable-span generation, and
  translation fetch coherence;
- `../engine/src/translator/cache.c` for code-cache publication, concurrent
  invalidation, stop-the-world acknowledgement, and fork repair;
- `../engine/src/host/linux/host.c` and
  `../engine/src/host/windows/memory.c` for host reservation, mapping,
  protection, discard, synchronization, address-range operations, W^X code
  aliases, and platform teardown.

The retained state is split among the guest mapping ledger, private-anonymous
protection and fork-policy registries, file/BUS intervals, logical shared
backings, executable generations, and translated-code cache.  Registry updates
are serialized by their owning locks; logical backing references survive guest
descriptor close and are released only after the last view or pin.  Mapping
changes publish guest accessibility before execution resumes and invalidate
affected translations.  Blocking guest-copy operations pin canonical storage,
return a partial count after transferred bytes, otherwise `EFAULT`, and release
pins on every exit.  Fork snapshots private anonymous state, retains shared
backings, applies `DONTFORK`/`WIPEONFORK`, and repairs executable aliases and
cache state in the child.  Teardown removes mapping, protection, BUS, fork,
lock, and SMC ownership together.

Linux-visible ordering and error behavior includes overflow and alignment
validation before mutation, exact `EINVAL`/`EFAULT`/`ENOMEM`/`EEXIST` results,
`MAP_FIXED` replacement, `MAP_FIXED_NOREPLACE`, split unmap/protection,
`MREMAP_FIXED` and `MREMAP_DONTUNMAP`, shared writeback versus private COW,
partial EOF pages followed by `SIGBUS`, zero-length syscall distinctions,
`MADV_DONTNEED`/`REMOVE`/fork policies, residency and lock accounting, and SMC
retirement after executable mapping changes.  AArch64-specific coverage owns
logical aliases, pair writeback, exclusive/LSE atomics, cross-view accesses,
and targeted instruction-cache retirement.  AMD64-specific coverage owns REP
fault restart state, LOCK/REP SMC boundaries, and concurrent executable alias
rewrites.  The retained macOS path deliberately weakens some protection and
fault behavior because of host VM granularity; those cases remain explicitly
`unsupported`.  The known NX and edge-fault fidelity gaps remain explicitly
`broken` rather than disappearing from discovery.

Rust ownership maps guest-visible regions, protection, placement, shared
objects, generations, reservations, atomic access, checkpoint values, and
mapping transactions to `src/runtime/hl-memory`.  Cross-domain descriptor,
file-backed mapping, futex, fork/exec, and execution-memory composition belongs
to `src/runtime/hl-runtime`; host mappings and the complete engine adapter are
selected in `src/containers/hl-engine`; native W^X publication and fault entry
remain in `src/native/execution`.  The corpus therefore records several honest
remaining integration gaps while keeping the reusable memory ledger free of
descriptor, syscall, and product policy.

## Native engine verification, 2026-08-03

The typed runner was invoked for AMD64 with
`HL_COMPAT_ENGINE_OPTIONS='HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1'`.
An initial broad run at 16 workers reported 61 passes and 26 failures.  The
failed cohort was then resumed at four workers with the same 30-second per-case
bound; `gna-negative-cache` passed on the bounded rerun, leaving 62 passes and
25 reproducible failures.  Native summary and detail diagnostics were emitted,
so this is engine evidence rather than QEMU-provider evidence.

The reproducible timeout cases are `anon-tracker-concurrent`,
`anonymous-mapping-reclamation`, `dbt-codecache-churn`,
`dbt-conc-mmap-exec`, `dbt-ibtc-mega`, `dbt-longjmp-reenter`,
`dbt-soak-mix`, `dbt-sparse-fault`, `elf-rodata-fault`,
`memfd-exec-alias-race`, and `memfd-offset-alias`. The following returned exit
status 1 instead of 0: `dbt-smc-grow`, `dbt-smc-minijit`,
`fixed-file-protection`, `fixed-noreplace`, `memfd-exec-alias`,
`syscall-logical-uaccess`, and `truncate-peer`. The reclamation cases
`allocator-reclamation`, `fd-reclamation`, `file-mapping-reclamation`,
`fork-reclamation`, and `thread-reclamation` produced their RSS measurement on
stderr, which the declared contract rejects; `zz-iso-flag` likewise produced
its diagnostic sequence on stderr. Finally, `guard-page-efault` returned
`ecwd=14` where the checked golden requires `ecwd=34`. These 25 cases are typed
broken on direct engine evidence. Their status is independent of the QEMU
oracle results recorded separately in `QEMU_EVIDENCE.md`.

The corresponding ARM64 active-set run was started with the same typed native
options and eight workers. It emitted native diagnostics, then aborted during
early execution with `*** stack smashing detected ***` after guest fault entry
at address `0x8ffc` (process exit 134). No ARM64 case-level pass summary was
produced, so this run is recorded as an engine activation/teardown blocker and
is not used to promote any case to active.
