# dd workspace checkpoint / restore (native CRIU-equivalent)

**Goal:** pause/resume a whole workspace at the lowest level — on close, freeze the guest
(processes + RAM + fds) to disk; on reopen, thaw it back exactly. Also lets the docker daemon
*pause* containers instead of killing them. User direction (2026-07-06): go straight for the
**disk dump** (survive close→reopen, free RAM), not just an in-RAM SIGSTOP pause.

## Why not criu
The reference design (`/Users/x/orbp/repo`, OrbStack-based) shells out to the real **`criu`
binary inside a Linux VM** (`orbp-guest/src/criu.rs`: `criu dump -t <pid>` / `criu restore`).
dd has **no Linux kernel** — guests run in-process on macOS via the JIT — so criu (ptrace,
`/proc`, freezer cgroup) cannot run. But dd **is** the kernel for its guests: it owns every
guest page, the CPU context, and the fd table, and sees every syscall. So checkpoint/restore is
implemented **natively in the engine** (dd-jit-darwin), snapshotting at a guest block boundary.
Cleaner than criu — no reverse-engineering of state from outside.

## Key architectural facts
- Same-ISA JIT in the engine's own address space → **guest VA == host VA** (guest pointers are
  real host pointers). For restore, force **fixed image bases** (`g_force_base`, `elf.c:704-717`;
  pcache already pins `PC_IMG_BASE=0x40000000000`, `PC_INTERP_BASE=0x48000000000`) so pages
  `MAP_FIXED` back to the same addresses and any baked pointers stay valid.
- Each guest **process** = a distinct **host process** (real `fork()`, `proc.c:1302`); each guest
  **thread** = a host `pthread` sharing the address space (`spawn_thread`, `thread.c:808`).
- The JIT translation cache is **not** checkpointed — re-translate on restore (pcache already
  persists it separately, `translate/aarch64/pcache.c`).
- **No checkpoint/restore code exists yet.** Closest templates: the fork-server pristine-image
  snapshot (`os/linux/forkserver.c:109-118,380-387` — image-span memcpy + W^X reapply) and pcache
  (fixed-VA serialization discipline, atomic temp+rename).

## Checkpointable surface (from the engine-state audit)

### 1. Guest memory — the RAM dump
- `g_gmap[8192]` (`os/linux/container/vfs/gmap.c:15`, `{addr,len,glen}` + `g_ngmap`): master list
  of every image seg / interp / heap / anon+file mmap. **Walk this to dump RAM.** Lacks prot/backing.
- `g_anonmap[2048]` (`os/linux/syscall/helpers.c:786`, `{addr,len,prot}`): per-region prot for
  anon/private ranges. File-backed prot comes from ELF phdrs (`vfs.c:maps_phdr_segs`, `:1170-1191`).
- Heap arena: 256 MB anon in `run_loaded` (`targets/linux_aarch64.c:586`); live `[brk_lo,brk_cur)`.
- Main stack: `g_stack_lo/g_stack_hi` (`elf.c:805-806`) — not gmap-tracked.
- Model the dumper on `proc_maps_fd()` (`vfs.c:1224-1269+`) — it already enumerates phdrs + g_gmap
  + stack + heap and sorts them (the exact pass needed, minus prot/backing metadata).

### 2. Guest CPU — `struct cpu` (`include/cpu_aarch64.h:8-79`)
Architectural state to save: `x[31], sp, pc, tls, nzcv, v[64]` (V0–V31), `sigmask`, `tid`,
`tpending`, `alt_sp/alt_size/alt_flags`, `ctid`. Skip (recomputable): shadow stack `sstk/ssp/gsp`
(reset via `G_SHADOW_RESET`), `vdirty/smc_va/irq/exited`. Enumerate all live CPUs via
`g_threg[4096]` (`thread.c:596`). Main-thread cpu is a **stack local** in `run_loaded:591`.
x86_64: `include/cpu_x86_64.h`.

### 3. fd table — the hard part (~40 host-fd-indexed side arrays, guest fd == host fd)
- Path-recoverable (reopen by path, **re-seek offset**): `g_fdpath[1024][192]` (`fscache.c:95`),
  overlay dirs `g_ovldir`, O_PATH `g_opath`.
- Pathless kernel objects — must **rebuild from the arrays** (same problem as rebuild-after-fork):
  - memfd/tmpfs scratch `g_memf[1024]` (`vfs.c:264`, in-RAM buffers), `g_memfd_is/seal`.
  - ptys `g_fd_ptsn/ptsmaster`, termios/winsize `g_ptm_*` (`fs.c:3281`, `state.c:134`).
  - sockets `g_sock_*` + loopback/tcp-listen/bridge/dns/netlink state (`state.c:306-474`, `fs.c:1818`).
  - epoll (kqueue-backed, **does not survive fork**; `kqueue_rebuild_after_fork`, `proc.c:207`):
    `g_epoll`, `g_ep_*` (`event.c:924`, `io.c:27`).
  - timerfd `g_timerfd/g_tfd_deadline/g_tfd_interval` (`event.c:492` — absolute deadlines, re-arm).
  - inotify `g_inotify/_wpath/_snap/_owner` (`event.c:494`); signalfd self-pipe `g_sigfd_*`
    (`signal.c:222`); eventfd `g_eventfd_*`; pipes `g_pipesz` + pushback `g_fd_pushback`; flock/
    record locks `g_flock_type/g_lkdev/g_lkino/g_lkval` (`fs.c:181`).
- The docker socket / bind volumes / overlay files are ordinary host regular-file fds → path+offset.
- **Unlinked / O_TMPFILE / memf-spill fds have no path** → dump their bytes inline.

### 4. Process model / consistent freeze
- Multi-process guest = tree of host engine processes; registry published under
  `/tmp/.ddpids.<key>/` (`proc_reg_publish`, `vfs.c:1569`).
- Quiesce primitive already exists: `cpu->irq` async-poll flag checked at every block boundary
  (`maybe_deliver_signal`, `signal.c:298`), plus `THREAD_INT_SIG=SIGINFO` to bounce siblings out of
  blocking host syscalls (`thread.c:644`). Reuse as tree-wide "stop at safe boundary".
- `fork_child_hooks` (`proc.c:176`) is the **checklist of fork-fragile state** (== restore-fragile):
  JIT re-alias, kqueue rebuild, futex/threg/sysv/poslk reinit, mlock drop, sigaltstack + Mach exc
  port, WIPEONFORK.

### 5. Other kernel state
- Signals: handlers `g_sigact[65]` (`signal.c:16`); pending `g_pending` + `cpu->tpending` + queued
  `g_sigcode/val/pid/uid/addr[65]`.
- Futex: `g_fbk` in a **MAP_SHARED** anon region (`thread.c:120`) — cross-process; dump once per
  container. Parked waiters are transient (recreated when guests re-issue FUTEX_WAIT).
- SysV IPC: per-container control block as a named POSIX shm (`sysv.c:190`), each segment its own
  named object — dump once per container, outside any process address space.
- cwd `g_cwd`; identity `g_uid/g_gid/groups/g_hostname/g_init_hostpid`; overlay/mount/network are
  **config-derived** (reconstruct from container config, don't dump).

### Engine entry points (where to hook)
`main`→`ddjit_entry`→`dd_run` (`targets/linux_aarch64.c:721/725/604`). `dd_run`:
`container_init` → `engine_global_init` → `load_program` → `run_loaded` (`:315/400/536/585`).
Dispatcher loop `run_guest` (`engine/dispatch.c`) — insert a "checkpoint requested" poll next to
the `cpu->irq` async-poll.

## Phased plan
- **P0 (unblock, tiny):** reap-on-close is already the win — engines leaking on interrupt is what
  wedges locks (proven: a leaked engine held debconf's `fcntl` lock). Ensure clean teardown.
- **P1 in-RAM pause:** `Runtime::pause()/resume()` = tree-wide SIGSTOP/SIGCONT via the process
  registry; daemon pauses container engines the same way. (User chose to skip straight to P2, but
  this is a trivial safety net.)
- **P2 disk dump (chosen):** at a quiesced block boundary, walk `g_gmap`(+prot) → dump pages;
  dump each `g_threg` cpu; serialize the fd side-tables; write a manifest. Exit (free RAM). On
  reopen: `container_init` from config → `MAP_FIXED` pages back → restore cpus → reopen/rebuild fds
  → resume `run_guest` at saved pc. Single-process, path-backed fds first; ptys/socket to the
  terminal reconnected like fork's kqueue rebuild.
- **P3 full fidelity:** multi-process trees (coordinate MAP_SHARED futex/IPC/ptrace arenas), all
  pathless fd types, docker daemon's container set.

## On-disk format (proposed)
A checkpoint dir per workspace: `MANIFEST` (json: arch, bases, brk/stack bounds, cpu count, fd
count, container config hash), `pages.bin` (concatenated region blobs, each `{addr,len,prot}` +
bytes), `cpu.N.bin` (per-thread `struct cpu` architectural subset), `fds.bin` (typed fd records).
Atomic temp+rename like pcache; poison/identity checks so a mismatched image refuses restore.
