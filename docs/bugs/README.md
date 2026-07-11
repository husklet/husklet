# Known Gaps

The single record of what is still missing. Everything else in the 414-finding
2026-07 audit was fixed and gated (full matrix: **1700 passed / 0 failed / 13
documented xfail**, all three engines). Each xfail is either one of the gaps
below or a broken-oracle artifact (qemu-user lacking a syscall) — see the
`dd-tests` case comments.

## Rendering (the active goal: Chrome renders web content on screen)

- **Wall 7 — MULTI-process command-buffer content still lost (futex fix landed
  and live-validated; it was necessary but NOT sufficient).** The shared-futex
  key fix (`bc9aa532`: file-backed MAP_SHARED words canonicalized to
  (st_dev, st_ino, offset); gate `futex-shared-key` 2/2) is confirmed live:
  **single-process Chrome now renders web content on the Metal path** (runs
  `chromium-run-gpu_retry_205503`/`205931`, content signature
  `Begin target=512/514` + page-bg clear `[0.914,0.933,0.969]` in the teed IR,
  page text visible in the frame PNGs). **Multi-process is still blank**: two
  independent post-fix runs (`202521`: 6589 frames, `210317`: 4221 frames)
  show zero content signature — only `Begin target=1` UI passes + the white
  `ClearRect{texture:1}` placeholder. UI stays live, so the GPU service's own
  channel works; specifically the renderer's raster stream never arrives.
  Narrowed (diagnostic run `211107`, `DD_FATALSIG_LOG=1`): no guest process
  dies of a fatal signal; the renderer is fully DORMANT — 100% of a 2s sample
  parked (7 threads in guest FUTEX_WAIT, 2 in the epoll/kevent Mojo pumps)
  while the browser paints 24 fps. The break is inbound event delivery to
  child guest processes over Mojo's fd transport (socketpair write → peer
  epoll wake / data-pipe eventfd), or the sync EstablishGpuChannel reply the
  renderer blocks on at startup. A recurring `Network service crashed,
  restarting service.` (engine sees no fatal signal) points at the same
  channel-level gap. Full context + how-to-run:
  [../rendering/README.md](../rendering/README.md).
- **(Resolved on branch render/chrome-content) engine launch drops libobjc
  fork-safety suppression.** Guest fork() is a real host fork() of a
  multithreaded objc-using engine process and guest execve() reloads in-place,
  so without `OBJC_DISABLE_INITIALIZE_FORK_SAFETY=YES` in the engine's exec env
  a guest fork racing another thread's `+initialize` aborts the child on first
  Foundation use (live signature: Chrome exits 137 right after
  `gl_shim: surface_up` with the `+[NSPlaceholderString initialize]` message;
  this — not FUTEX_WAIT — was killing the post-fix single-process runs
  launched from macOS, whose BSD-`script` harness branch dropped the var).
  Fix: `ddcli workspace launch` guarantees the var
  (dd-cli/src/ddjit_launcher.rs).
- **Content orientation is per-capture variable.** The offscreen-pass Y-flip
  heuristic renders the old `chrome-stream-ir-000.ir` upright but the new
  single-process captures upside-down (identical through main's and the
  integrated branch's backend, so it is Chrome choosing a different composite
  transform, not a dd-display regression). Orientation must be derived from
  the pass's projection/viewport transform, not a static offscreen rule —
  orientation workstream (`render-integrated`).

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
