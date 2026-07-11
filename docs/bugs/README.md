# Known Gaps

The single record of what is still missing. Everything else in the 414-finding
2026-07 audit was fixed and gated (full matrix: **1700 passed / 0 failed / 13
documented xfail**, all three engines). Each xfail is either one of the gaps
below or a broken-oracle artifact (qemu-user lacking a syscall) — see the
`dd-tests` case comments.

## Rendering (the active goal: Chrome renders web content on screen)

- **Wall 7 — cross-process command-buffer content lost. ENGINE FIX LANDED,
  LIVE VALIDATION PENDING.** dd keyed futexes by host VA; a file-backed
  MAP_SHARED word (Chrome's renderer↔GPU command buffer) maps at different VAs
  per process, so the wake landed in the wrong bucket and was dropped. Fixed:
  shared futex words are canonicalized to (st_dev, st_ino, offset); regression
  gate `futex-shared-key` passes (was woke=0/0, now 1/1). Still to do: run
  live multi-process Chrome (`CHROME_TIMEOUT>=180`) on the macOS Metal
  pipeline and confirm the content signature in a fresh `freshir-*.ir`
  (offscreen tile `Begin target=512/514` + page-bg clear `[0.914,0.933,0.969]`)
  / the oracle page on screen. If content is still blank, instrument the next
  wakeup channel (Mojo data-pipe) the same way. Full context:
  [../rendering/README.md](../rendering/README.md).
- **Single-process first-commit idle-stall.** `--single-process` Chrome (which
  DID render content Jul-9) parks in FUTEX_WAIT before the first wl_surface
  commit. Likely the same shared-futex-key mechanism (two mappings of the same
  memfd word in one process — the fixed `two_map` case). RETEST on the fixed
  engine before treating as a separate bug; if still stalled, per-tid backtrace
  at the stall.

## Engine — W^X / page-permission cluster (approach explicitly halted)

Fixing these requires real host-page protection plus a fault-handler layer
that must coexist with the JIT's own SIGSEGV handling; the attempt was stopped
deliberately. Tracked by the `syscall/mprotect` xfail pair (auto-XPASS when
implemented).

- Guest `PROT_NONE` mappings remain directly readable (syscall-arg paths
  EFAULT correctly via the intent registry; direct guest loads do not fault).
- Writes to `mprotect(PROT_READ)` pages do not fault.
- Execute permission is not enforced for guest fetch (the JIT translates and
  runs code from non-executable mappings).

## Engine — Apple-Silicon 16K page size

Software-fixable (dd relies on bookkeeping, not hardware protection, for guest
semantics): report `AT_PAGESZ=4096` and emulate 4K subpages over 16K host
pages in the gmap/gna registries, issuing the real host munmap only when a
full 16K page clears. Tracked by the `completeness/auxval` xfail (auto-XPASS).

- aarch64 `AT_PAGESZ` exposes the host 16K page (guests expect 4096).
- aarch64 4K-subpage `munmap` returns `EINVAL` (host granularity).

## Engine — untrusted (sentry) mode

- **Untrusted split breaks Linux `EFAULT`.** Worker marshaling in `sentry.c`
  memcpy's guest pointers before validation (e.g. cases 63/64/25, ~line 1590)
  — bad pointers crash the worker on aarch64 / ship wrong data on x86_64
  instead of returning -EFAULT. Fix: guard every guest-pointer deref in the
  ring marshaling with the trusted path's `guest_bad_ptr`/`host_range_mapped`.

## Engine — syscalls

- **`F_SETLEASE` lease-break signal not delivered** (residual; `F_NOTIFY`
  works). Design ready: shared cross-process lease table modeled on the
  `poslk` registry (helpers.c), open-path conflict hook in fs.c case 56,
  break signal via `kill(holder_hostpid, sig_l2m(sig))`, holder-liveness
  cleanup like the cgroup acct table.
- **Darwin jail symlink semantics can produce wrong contents** (macOS
  containers). Correct fix is an overlay/volume-aware securejoin resolver on
  `jail()`'s hottest path; cannot be runtime-verified from the Linux dev host.

## Launch / daemon

- **Typed-launch path lists: delimiter escaping** (residual; the cross-arch
  `DD_LOWER` delimiter mis-split is fixed). A `DDVOL`/`DD_LOWER` source path
  containing `:` or `,` still mis-splits. The fix must update every raw
  producer (wire.rs joiner, harness `.env`, CLI `add_vol`) in lockstep with
  escape-aware C parsers; backslash-escaping keeps delimiter-free paths
  byte-identical.
- **Fractional `--cpus` loses quota precision.** `--cpus=1.5` must yield
  `cpu.max 150000 100000`. The wire header is growable (proven by the
  `egress_off` 112→120 change; Rust `to_wire` + C `ddjit_config` compile into
  one artifact) — carry the precise quota in a new/`reserved0` field.
- **Peer `/proc/<pid>/fd/<n>` open** (residual; listing/readlink/stat work via
  libproc). Actually opening a peer's fd needs cross-process fd passing
  (SCM_RIGHTS-level) between engine workers.
- **`docker logs -f` can drop output for slow clients.** Correct fix drives
  the follow stream from the retained `log_chunks` buffer instead of the lossy
  broadcast channel (a follow-task rewrite; the race is hard to verify).
