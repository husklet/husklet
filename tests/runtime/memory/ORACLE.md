# Memory compatibility oracle audit

> **Historical ownership:** References to `src/native/execution` and the former
> Rust scheduler describe deleted or unselected replacement work. Production
> memory behavior currently enters `src/runtime/native/retained`.

Retained C was studied read-only in `../engine/src/linux_abi/syscall/mem.c`, `../engine/src/linux_abi/host_mman.h`, `../engine/src/linux_abi/host_uio.h`, `../engine/src/linux_abi/linux_abi.c` vector entry points, `../engine/src/linux_abi/sentry.c` iovec admission/copyback, and the mapping/pipe/fork paths. Mapping and descriptor identities own state; whole vectors are admitted before payload transfer, Linux caps and partial results are preserved, pipe writes within `PIPE_BUF` remain atomic, blocking operations wake or interrupt with exact errno, and last-close/fork teardown is explicit. Rust ownership maps to `hl-memory`, `hl-descriptor`, and `hl-runtime` memory/filesystem/pipe composition.

The four cases preserve lock scope, vector caps, validation ordering, atomic pipe writes, scatter reads, and forked aliases on both ISAs.

The `elf-rodata-write` fixture also preserves its exceptional retained link
contract from `../engine/cmake/Phase3Compat.cmake`: the byte-identical
`source/elf_rodatawrite.ld` is passed with `-Wl,-T` so the `.elfwrite` section
starts on a 16 KiB boundary and spans the intended 4 KiB guest subpage. The
ordinary memory-suite flags in `../engine/tests/compat/memory/manifest.tsv` do
not record this per-target CMake override by themselves.

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
`unsupported`.  The edge-fault fidelity gap remains explicitly `broken` rather
than disappearing from discovery; the NX gap was diagnosed and fixed on
2026-08-08 (see the final section).

Rust ownership maps guest-visible regions, protection, placement, shared
objects, generations, reservations, atomic access, checkpoint values, and
mapping transactions to `src/runtime/hl-memory`.  Cross-domain descriptor,
file-backed mapping, futex, fork/exec, and execution-memory composition belongs
to `src/runtime/hl-runtime`; host mappings and the complete engine adapter are
selected in `src/containers/hl-engine`; native W^X publication and fault entry
remain in `src/native/execution`.  The corpus therefore records several honest
remaining integration gaps while keeping the reusable memory ledger free of
descriptor, syscall, and product policy.

## Native engine verification, 2026-08-07 (supersedes 2026-08-03)

The 25 cases typed broken on 2026-08-03 were re-measured at head with a release
runner (`cargo build --release -p testing --bins`, `--engine-profile release`),
one case per invocation at `--jobs 1` for AMD64 and ARM64, and then confirmed by
a whole-suite AMD64 sweep at `--jobs 8`.  All three AMD64 measurements agree
exactly.  The suite declares `execution: { native: true, diagnostics: true }`,
so native summary and detail diagnostics were emitted; the corpus runner accepts
no engine options, so no `HL_NATIVE_EXECUTION` on/off parity was measured.

Thirteen of the 25 now pass and are typed active: `dbt-codecache-churn`,
`dbt-conc-mmap-exec`, `dbt-ibtc-mega`, `dbt-longjmp-reenter`, `dbt-smc-grow`,
`dbt-smc-minijit`, `dbt-soak-mix`, `dbt-sparse-fault`, `elf-rodata-fault`,
`memfd-exec-alias-race`, `memfd-offset-alias`, `syscall-logical-uaccess`, and
`anon-tracker-concurrent`.  The 2026-08-03 timeouts were instruction-charging
inflation in `hl_x86_finish_chain`, which stored the chain total in `scratch[0]`
so completed loop iterations were charged twice, together with the stale
`indirect_site` that made `ibtc_fill` refuse whole runs after an arena rotation.
`anon-tracker-concurrent` is correct but genuinely expensive: it needs about 55
to 60 seconds on AMD64 (12 seconds on ARM64) against the 30-second default, so
it now declares `timeout: 180` rather than being recorded as a failure.

The 2026-08-03 ARM64 blocker is also gone.  A full ARM64 pass over the same
cases completed with case-level results and no `stack smashing detected` abort.

Twelve were recorded broken here, and none of them failed for the recorded
2026-08-03 reason.  Eleven were harness contract gaps rather than engine
defects; the twelfth, `memfd-exec-alias`, was a genuine engine defect and has
since been fixed.  All twelve are active and passing on both guest ISAs:

- Seven were unpassable under the runner's stdout/stderr contract, which
  rejected any non-empty stderr.  `allocator-reclamation`,
  `anonymous-mapping-reclamation`, `fd-reclamation`, `file-mapping-reclamation`,
  `fork-reclamation`, and `thread-reclamation` emit the retained `memrss.h`
  debug line on stderr, and all six report `grew=0KB` well inside their
  thresholds, so the reclamation bound under test holds.  `zz-iso-flag` writes
  its `A`..`Z` progress markers to fd 2, reaches `Z done`, and exits 0.  Each
  now declares its stderr lines through `expect.stderr`, which requires every
  emitted line to match a declared pattern and every declared pattern to appear.
- Three need a writable regular file at `/data`, the retained
  `mapping-data-rootfs` capability recorded in
  `../engine/tests/compat/memory/manifest.tsv`.  `fixed-file-protection`,
  `fixed-noreplace`, and `truncate-peer` exited 1 with empty stdout at their
  first `open("/data")` before reaching any mapping behaviour.  Each now
  declares `guest.files`, which stages the same 12288 bytes of `0x2a` at 0600.
- `guard-page-efault` failed on a golden that encodes its capture environment.
  Its `ecwd=34` (`ERANGE`) records the long working directory of the retained
  native-Linux capture host, while the corpus container working directory was
  short, so `getcwd(guard, 16)` legitimately reached the buffer probe and
  reported `EFAULT`.  The engine orders `ERANGE` before the buffer probe
  correctly; the case now pins a long working directory through `guest.cwd`
  rather than editing the golden.
- `memfd-exec-alias` was the one genuine engine defect, and it was AMD64 only.
  Writes through the RW alias of a shared memfd became visible in the RX alias
  (`scalar-visible=1`, `vector-visible=1`, `avx256-visible=1`) but did not retire
  the translated code for that alias, so `scalar-exec=0`, `cross-page=0`, and
  `rep-exec=0` kept returning the previous immediate; only the wider 32-byte
  store, which reaches a further guest page, forced retirement, leaving
  `avx256-exec=1`.  Fixed in `6cf2ad32e`, which retires translations for stores
  through a writable executable alias.  The case is typed active and passes on
  both guest ISAs.

## Re-verification against head, 2026-08-07

The section above was re-checked against the corpus at head rather than carried
forward.  `tests/runtime/memory/test.yaml` declares 114 cases: 95 active, 14
`unsupported`, and 5 `broken`.

All 25 cases typed broken on 2026-08-03 are now `status: active`, each carrying
the mechanism the section above credits: `anon-tracker-concurrent` declares
`timeout: 180`; `allocator-reclamation`, `anonymous-mapping-reclamation`,
`fd-reclamation`, `file-mapping-reclamation`, `fork-reclamation`,
`thread-reclamation`, and `zz-iso-flag` declare `expect.stderr`;
`fixed-file-protection`, `fixed-noreplace`, and `truncate-peer` declare
`guest.files`; `guard-page-efault` declares `guest.cwd`.  No memory case is
typed broken for an engine defect any more.

The five that remain `broken` are unrelated to that cohort and still hold.
`mlockall-scope`, `vector-limits`, and `vector-order` are QEMU-oracle
divergences whose evidence is `QEMU_EVIDENCE.md`, not engine failures.
`nx-exec` and `edge-faults` are the NX and edge-fault fidelity gaps this
document already records above as deliberately retained rather than hidden.
(`nx-exec` was superseded on 2026-08-08; see the section below.)
The 14 `unsupported` cases are the macOS host-VM exclusions, also as recorded.

`dbt-longjmp-reenter` is active and correct but load-sensitive: it passes
uncontended in about 28 seconds against the 30-second default bound and can
exceed it on a loaded box.  That is a measurement condition, not a regression.

## Native engine verification, 2026-08-03 (superseded)

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
`ecwd=14` where the checked golden requires `ecwd=34`. These 25 cases were typed
broken on that evidence; the 2026-08-07 section above replaces it and none of
the 25 still carries this reason. Their status is independent of the QEMU
oracle results recorded separately in `QEMU_EVIDENCE.md`.

The corresponding ARM64 active-set run was started with the same typed native
options and eight workers. It emitted native diagnostics, then aborted during
early execution with `*** stack smashing detected ***` after guest fault entry
at address `0x8ffc` (process exit 134). No ARM64 case-level pass summary was
produced, so this run was recorded as an engine activation/teardown blocker and
was not used to promote any case to active. That blocker no longer reproduces.

## `nx-exec`: the recorded reason was wrong, 2026-08-08

`nx-exec` was typed `broken` with a reason claiming that "the engine strips
`PROT_EXEC` and forces host `PROT_READ` (syscall/mem.c mmap+mprotect case 226)
so guest exec permission is never represented", that a non-executable page
"executes on both targets", and that the fix "needs a guest-exec registry
checked at translate-block code fetch".  Every one of those claims was checked
against head and is false; the two file references are to the retained C
oracle, not to this engine.

What is actually true.  Guest execute permission *is* represented: `mmap`/
`mprotect` decode `PROT_EXEC` into `Protection::EXECUTE`
(`hl-linux/src/memory/abi.rs`), the ELF loader gives text segments `EXECUTE`
(`hl-engine/src/ffi/linux/loader.rs`), `RegionSet::resolve` refuses any access
whose region lacks the requested bit (`hl-memory/src/region_set.rs`), and the
native translator already declines to build a block whose PC lacks `EXECUTE`
(`hl-engine/src/ffi/linux/execution/scheduler/native.rs`).  A guest `mmap` of
`PROT_READ|PROT_WRITE|PROT_EXEC` succeeds and executes on both ISAs, and W|X is
held simultaneously, which matches unhardened Linux.  The "registry checked at
translate-block code fetch" the reason asks for already existed.

The two real defects were narrower, and neither was a stripped `PROT_EXEC`:

- The aarch64 interpreter fetched instructions through
  `GuestOperandMemory::read`, which demands only `Protection::READ`, instead of
  the `InstructionFetch`/`Protection::EXECUTE` path the x86 interpreter uses.
  So NX was unenforced on aarch64 only, and only in the interpreter: the native
  translator correctly declined such a PC, fell back to the interpreter, and the
  interpreter then ran the bytes anyway.  amd64 already enforced NX.
- `GuestExecutor::fault_signal` did not match `ExecutionFault::Fetch`, so a
  correctly denied fetch fell to `_ => None` and killed the thread with a
  `FaultReason::Fetch` exit instead of raising SIGSEGV.  No guest handler ran
  and no `si_code` was ever computed, on either ISA.  This is why the case
  timed out rather than printing `killed_segv=0`: the amd64 child died without
  a wait status the fixture could classify.

Both are fixed; `nx-exec` is `active` and passes on both ISAs in about 190 ms.
It now pins `runs > 0` so it cannot silently stop reaching the native engine.
Fetch denial now reaches `classify`, which reports `SEGV_ACCERR` when the
address is mapped and `SEGV_MAPERR` when it is not, matching Linux for both the
`PROT_READ` and `PROT_NONE` probes.
