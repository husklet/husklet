# Engine status and remaining gaps

Status: current consolidated backlog, 2026-07-12.

Rendering and Chrome status live in [`codex-rendering.md`](codex-rendering.md) and
[`rendering/CHROME-FIX-PLAN.md`](rendering/CHROME-FIX-PLAN.md). This file covers the JIT engine,
Linux-compatibility layer, launch wire, and daemon only.

## Open engine gaps

### Guest page permissions and W^X

The engine tracks enough mapping intent to validate many syscall pointers, but it does not enforce guest page
permissions for direct translated loads/stores/fetches:

- direct reads from guest `PROT_NONE` mappings do not fault;
- writes through `mprotect(PROT_READ)` mappings do not fault;
- instruction fetch from non-executable mappings is not rejected.

The behavioral `syscall/mprotect` cases remain xfail and should auto-XPASS when implemented. A fix needs host-page
protection/subpage bookkeeping integrated with the JIT fault handler, not more syscall-only validation.

### Apple-Silicon 16 KiB host pages

Linux guests expect 4 KiB pages, while arm64 macOS exposes 16 KiB pages. Remaining observable gaps:

- `AT_PAGESZ` reports the host page size instead of 4096;
- a guest 4 KiB subpage `munmap` can return host `EINVAL`.

Implement 4 KiB guest subpage state over 16 KiB host mappings and release the host page only when all guest
subpages are unmapped. The `completeness/auxval` case tracks this behavior.

### x86 floating-point fidelity

- The x87 register stack uses a 64-bit `double` carrier. Inf/NaN m80 conversion is fixed and covered by `x87m80`,
  but true 80-bit mantissa/exponent precision remains absent. Correctness requires a software ext80 representation
  and differential long-double tests; it cannot be fixed by widening the host `long double` on arm64 macOS.
- x86 `DIVSS`/`DIVPS` (and scalar/double relatives) can differ from arm64 for the sign bit of default NaNs produced
  by `0/0` or infinity/infinity. `fpdnan` covers the currently supported value classes; preserve bit-exact x86 NaN
  behavior where observable.

### Untrusted/sentry pointer validation

The sentry worker can dereference guest pointers while marshaling requests before applying the trusted path's
`guest_bad_ptr`/`host_range_mapped` validation. Invalid Linux pointers must return `EFAULT`, not crash a worker or
copy unrelated host data. Apply validation to every pointer-bearing ring request and add cross-engine bad-pointer
behavioral tests.

### Syscall compatibility

- `F_SETLEASE` lease-break signals are not delivered (`F_NOTIFY` works). Implement a cross-process lease table,
  open-path conflict hook, signal delivery, and holder-liveness cleanup.
- Darwin jail symlink resolution can select wrong contents across overlays/volumes. The fix needs an
  overlay-aware secure-join resolver and macOS behavioral tests.

## Open launch and daemon gaps

- Typed launch path lists do not escape `:` or `,` in `DDVOL`/`DD_LOWER` sources. Update every Rust producer and C
  parser together with round-trip tests.
- Fractional `--cpus` loses quota precision; `1.5` must produce `cpu.max 150000 100000`. Carry exact quota through
  the versioned launch wire.
- Opening `/proc/<pid>/fd/<n>` for another engine process needs an SCM_RIGHTS bridge; listing/readlink/stat already
  work.
- `docker logs -f` can lose chunks for slow readers because it follows a lossy broadcast channel. Follow from the
  retained ordered `log_chunks` buffer and test a deliberately stalled client.

## Fixed dangerous-hole wave

The July 8 H-class wave closed these silent-success/corruption defects:

- `FUTEX_WAKE_OP`, PI/robust futex ownership and wake behavior;
- high-fd network table bounds, SEQPACKET/passcred/peercred/EOF handling, and real `setsockopt` errors;
- fallocate range operations, mount/umount/pivot-root behavior, and memfd grow/shrink seals;
- `mprotect(PROT_EXEC)` self-modifying-code invalidation;
- x86 MIN/MAX NaN/signed-zero, unordered compare masks, float-to-int indefinite results, runtime DF string ops,
  SHLD/SHRD flags, and m80 Inf/NaN conversion.

Behavioral cases remain registered under `smcmprotect`, `pi_robust`, `fpedge`, `shldflags`, `fpdnan`, `repmovsdf`,
`x87m80`, high-fd/network, fallocate, mount, seal, and futex groups. The historical recorded full-matrix result was
1642 passed / 0 failed / 13 documented xfail on all three engines. That number is historical, not a current CI claim;
use `make test` for current evidence.

## Maintenance rule

Keep only current gaps and concise closure summaries here. Investigation timelines, branch diaries, raw traces, and
screenshots belong in git history or review artifacts. A gap closes only with an observable behavioral regression,
not a source-string search.
