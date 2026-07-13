// dd/runtime/os/linux -- native checkpoint/restore ("CRIU-equivalent"), MULTI-PROCESS.
//
// Freezes a running guest -- a WHOLE process tree (multiple shells, background jobs, their children) -- to an
// on-disk directory (RAM + CPU + fds, per process), so every host engine process can exit and free its
// memory, then later resume the entire tree EXACTLY from where it left off. dd has no Linux kernel (guests
// run in-process on macOS via the JIT), so criu (ptrace, /proc, freezer cgroup) cannot run; but dd IS the
// kernel for its guests -- it owns every guest page, the CPU context and the fd table -- so checkpoint/
// restore is implemented natively in the engine, snapshotting at a guest block boundary.
//
// WHY THE MEMORY RESTORE IS ROBUST: this engine keeps every guest page host-RW and NEVER executes guest
// pages (translation READS their bytes; host code runs in a separate RX arena). So a restore can MAP_FIXED
// every region back as plain anon-RW and memcpy the saved bytes; RELRO/PROT_NONE intent is carried in the
// side registries (g_anonmap prot, g_gna) exactly as the live engine carries it. The translated-code arena
// is NOT persisted -- restore begins with an EMPTY block map (children are re-forked BEFORE the tree runs,
// so no stale translation is ever inherited) and re-translates from the restored guest bytes.
//
// MULTI-PROCESS MODEL (the core): a guest process tree == a tree of host engine processes (each guest
// fork/clone is a real host fork(), proc.c). CHECKPOINT is triggered by advancing a SHARED-MEMORY generation
// counter (a MAP_SHARED "<dir>.trigger" every engine process maps) -- NOT a signal, because a guest's own
// rt_sigaction silently remaps every guest-reachable host signal (bash SIG_IGNs SIGUSR1). ckpt_poll reads the
// generation at each safepoint; the engine's guest-clobber-proof THREAD_INT_SIG (SIGINFO) is reused only to
// KICK a process out of a blocking host syscall (EINTR) or a chained in-cache loop (thread_int_handler sets
// cpu->irq when armed) to its next safepoint. The container init (guest pid 1) is the coordinator: it
// enumerates the tree (every ENGINE process in its session -- robust vs the lossy pid registry), kicks each
// peer, waits for each to dump, then dumps itself + writes the MANIFEST (its presence == a complete
// checkpoint). Each process dumps its OWN private memory + cpu + fds to <dir>/proc.<gpid>/ (memory is
// COW-private per process, so per-process dumps need no cross-process coherency).
// RESTORE: rebuild the tree in ppid order (CRIU's proven ordering) -- the
// init restores its own RAM FIRST (before engine allocation, so MAP_FIXED lands on free VAs), then re-forks
// each child; each child resets its inherited registries, MAP_FIXEDs its OWN saved RAM, runs the shared
// after-fork engine reset (fork_child_hooks: cache re-alias, kqueue rebuild, lock/threg/Mach-port reset),
// reopens its own path fds (tty fds are INHERITED down the fork from the launcher's pty), recursively re-
// forks its own children, then resumes at its saved pc. A restore installs a PID-translation table
// (state.c g_pidmap) so guest-visible pids (a blocked wait4's target, a reaped child's pid, bash's job
// table) stay stable even though the re-forked tree has new host pids.
//
// ON-DISK FORMAT (checkpoint dir):
//   MANIFEST         : struct ckpt_manifest (magic/version/arch, process count, root gpid) -- written LAST
//   proc.<gpid>/     : one dir per guest process (published temp-dir + rename)
//     meta   : struct ckpt_meta (identity: self/ppid/pgid/sid gpid; brk/stack/nonpie bounds; exe path; ...)
//     pages  : [struct ckpt_region][region's non-zero pages: {u64 va}{pagesz bytes}] ...  (sparse)
//     cpu    : the whole per-thread struct cpu (host-transient fields zeroed on restore)
//     fds    : n_fds * struct ckpt_fd (TTY | FILE by host path + seek offset + open flags)
//
// TRIGGER (checkpoint): DDJIT_CHECKPOINT_DIR=<dir> arms a SIGUSR1 control handler (inherited by every forked
// guest process); SIGUSR1 to the container init checkpoints the whole tree then _exit()s each process.
// RESTORE: DDJIT_RESTORE_DIR=<dir> (or the `--restore <dir>` engine flag) -> dd_restore(rootfs, dir).
// dd_jit::Runtime layers checkpoint(dir)/restore(dir) on this; the GUI calls it on window close/reopen.

#include <libproc.h>   // proc_pidpath: filter session members to engine processes
#include <sys/sysctl.h> // KERN_PROC_SESSION: enumerate the container's whole process tree
#include <sys/wait.h>   // waitid/waitpid: coordinator peer-reap; multi-thread refusal probe

#define CKPT_MAGIC 0x325450434b444444ull          // "DDDKCPT2" (LE) -- per-process meta
#define CKPT_MANIFEST_MAGIC 0x324e414d504b4444ull // "DDKPMAN2" (LE) -- workspace manifest
#define CKPT_VERSION 4 // v4: meta carries the guest signal-disposition table (sig_*), re-installed on restore
#define CKPT_ARCH_AARCH64 2

#define CKF_TTY 1  // controlling terminal / any tty -- inherited down the restore fork from the launcher pty
#define CKF_FILE 2 // path-backed regular file -- reopened by host path + lseek to the saved offset

struct ckpt_manifest {
    uint64_t magic, version, arch;
    uint64_t n_procs;
    int32_t root_gpid;
    // The controlling terminal's FOREGROUND process group at checkpoint, in guest terms (1 == the container
    // init's own group; 0 == none/unknown). Restore re-points the fresh pty at it (tcsetpgrp) so ^C/^Z reach
    // the foreground job -- without it the resumed tree's fg group defaults to the init and ^C kills the tree.
    int32_t fg_pgid_gpid;
};

struct ckpt_meta {
    uint64_t magic, version, arch, engine_id;
    uint64_t cpu_sz, pagesz;
    uint64_t n_regions, n_threads, n_fds;
    uint64_t brk_lo, brk_cur, brk_hi;
    uint64_t nonpie_lo, nonpie_hi, nonpie_bias;
    uint64_t stack_lo, stack_hi;
    int32_t self_gpid, ppid_gpid; // guest identity: this process's pid + its parent's (0 for init's parent)
    int32_t pgid_gpid, sid_gpid;  // guest process group + session (1 == the container init's group/session)
    char exe_path[512];
    // Guest signal-disposition table (g_sigact[65]), captured per process. It is ENGINE C state -- not in
    // the guest RAM dump and not in struct cpu -- so a restored process would otherwise start all-SIG_DFL
    // with DEFAULT host dispositions, and a ^C (SIGINT) at a restored prompt would hit the host default
    // action (terminate) and KILL the shell instead of running its interrupt handler. Carried here and
    // replayed on restore (ckpt_reinstall_sigacts) so async signals route back through the engine handler.
    uint64_t sig_handler[65], sig_flags[65], sig_mask[65];
};

struct ckpt_region {
    uint64_t addr, len, glen;
    int32_t prot;   // guest-intent protection (from the anon registry; PROT_READ|WRITE default)
    int32_t is_gna; // 1 if this region is guest-PROT_NONE (rebuild the g_gna EFAULT registry on restore)
    uint64_t npages;
};

struct ckpt_fd {
    int32_t gfd, kind, flags, _pad;
    int64_t offset;
    char path[512];
};

// ---- control channel (armed only when DDJIT_CHECKPOINT_DIR / DDJIT_RESTORE_DIR is set) ----
// The checkpoint request is conveyed by a SHARED-MEMORY generation counter, NOT a signal: a MAP_SHARED
// mmap of "<checkpoint-dir>.trigger" (a sibling of the checkpoint dir, so the coordinator's rm of the dir
// never touches it). Every engine process maps it (inherited across fork, remapped after exec). ckpt_poll
// reads it each safepoint (a cheap memory load) and checkpoints when the generation advances past the one it
// last saw. Signals are unusable as the trigger because a guest's own rt_sigaction remaps every guest-
// reachable host signal (bash sets SIG_IGN on SIGUSR1, silently swallowing it); the ONLY guest-clobber-proof
// signals (macOS SIGEMT/SIGINFO, which sig_l2m never targets) are already taken by STW / THREAD_INT. So the
// generation carries the INTENT and the engine's existing guest-proof THREAD_INT_SIG (SIGINFO) is reused
// only to KICK a blocked/spinning process out to its safepoint (thread_int_handler sets cpu->irq when armed).
#define CKPT_KICK_SIG SIGINFO
static char g_ckpt_dir[1024];
// g_ckpt_trigger / g_ckpt_seen_gen live in container/state.c (early include) so signal.c's blocking-syscall
// restart decision (ckpt_pending) can consult them too.

static int ckpt_dump_self(struct cpu *c, const char *procdir);
static void ckpt_coordinate_and_exit(struct cpu *c);

// Map (creating if needed) the shared trigger-generation page for `dir`. Returns the mapping or NULL.
static volatile uint32_t *ckpt_map_trigger(const char *dir) {
    char tp[1200];
    snprintf(tp, sizeof tp, "%s.trigger", dir);
    int fd = open(tp, O_RDWR | O_CREAT, 0600);
    if (fd < 0) return NULL;
    if (ftruncate(fd, 4) != 0) { /* already sized is fine */
    }
    void *m = mmap(NULL, 4, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    return (m == MAP_FAILED) ? NULL : (volatile uint32_t *)m;
}

static int ckpt_wr_all(FILE *f, const void *buf, size_t n) { return fwrite(buf, 1, n, f) == n ? 0 : -1; }
static int ckpt_rd_all(FILE *f, void *buf, size_t n) { return fread(buf, 1, n, f) == n ? 0 : -1; }

static int ckpt_rmrf(const char *path) {
    DIR *d = opendir(path);
    if (d) {
        struct dirent *e;
        char child[2048];
        while ((e = readdir(d))) {
            if (!strcmp(e->d_name, ".") || !strcmp(e->d_name, "..")) continue;
            snprintf(child, sizeof child, "%s/%s", path, e->d_name);
            if (ckpt_rmrf(child) != 0) unlink(child);
        }
        closedir(d);
        return rmdir(path);
    }
    return unlink(path);
}

// Called at the top of the dispatcher loop (a clean safepoint: all guest arch state is spilled into `c`).
// Referenced by engine/dispatch.c via the G_CKPT_POLL seam (aarch64-only). Cheap: a NULL test + one shared
// memory load on the hot path. When the trigger generation advances, the container INIT coordinates the
// whole tree; a peer dumps only itself. Never returns once it fires (all processes _exit after snapshotting).
static void ckpt_poll(struct cpu *c) {
    if (!g_ckpt_trigger) return;
    uint32_t g = *g_ckpt_trigger;
    if (g == g_ckpt_seen_gen) return;
    g_ckpt_seen_gen = g;
    if (container_pid() == 1) {
        ckpt_coordinate_and_exit(c); // never returns (dumps the tree + _exit)
    }
    char pd[1200];
    snprintf(pd, sizeof pd, "%s/proc.%d", g_ckpt_dir, container_pid());
    int rc = ckpt_dump_self(c, pd);
    fprintf(stderr, "[ckpt] proc %d %s\n", container_pid(), rc == 0 ? "OK" : "FAILED");
    _exit(rc == 0 ? 0 : 70);
}

// Arm checkpoint/restore if DDJIT_CHECKPOINT_DIR / DDJIT_RESTORE_DIR is set. Called from engine_global_init
// (in every process, so a forked child is armed too). Maps the shared trigger and records the CURRENT
// generation as already-seen, so a stale trigger from a previous run never false-fires on a fresh launch or a
// restore (only a later increment triggers a checkpoint).
static void ckpt_control_init(void) {
    const char *rd = getenv("DDJIT_RESTORE_DIR");
    const char *d = getenv("DDJIT_CHECKPOINT_DIR");
    if ((d && d[0]) || (rd && rd[0])) g_ckpt_armed = 1;
    if (!d || !d[0]) return;
    snprintf(g_ckpt_dir, sizeof g_ckpt_dir, "%s", d);
    g_ckpt_trigger = ckpt_map_trigger(d);
    if (g_ckpt_trigger) g_ckpt_seen_gen = *g_ckpt_trigger;
}

// Is `fd` a GUEST-owned pathless kernel object (socket/pipe/epoll/eventfd/timerfd/inotify/memfd)? Tracked in
// the engine per-fd side-tables. A non-tty, non-regular fd absent from all of them is an ENGINE-internal
// descriptor (a global kqueue, the netns control socket, ...) the guest cannot see -- skipped. A guest-owned
// one is the P3 case ckpt_dump_self refuses cleanly.
static const char *ckpt_guest_kernel_fd(int fd) {
    if (fd < 0 || fd >= DD_NFD) return NULL;
    if (g_epoll[fd]) return "epoll";
    if (g_sock_stream[fd] || g_sock_dgram[fd] || g_sock_seqpacket[fd] || g_dns_sock[fd] || g_sock_fam[fd])
        return "socket";
    if (g_pipesz[fd]) return "pipe";
    if (g_timerfd[fd]) return "timerfd";
    if (g_inotify[fd]) return "inotify";
    if (g_memfd_is[fd]) return "memfd";
    if (g_eventfd_peer[fd]) return "eventfd";
    return NULL;
}

static int ckpt_live_threads(void) {
    int n = 0;
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c) n++;
    return n;
}

// ================================ CHECKPOINT (per process) ================================

// Snapshot every path-backed / tty guest fd into `recs`; REFUSE (return -1) on any GUEST-owned pathless
// kernel-object fd (P3). MUST run BEFORE any checkpoint output file is opened, so the writer's own fds are
// never mistaken for guest fds.
static int ckpt_scan_fds(struct ckpt_fd *recs, int cap, int *out_n) {
    int n = 0;
    for (int fd = 0; fd < DD_NFD && n < cap; fd++) {
        if (fcntl(fd, F_GETFD) < 0) continue; // not open
        struct stat st;
        if (fstat(fd, &st) != 0) continue;
        struct ckpt_fd r;
        memset(&r, 0, sizeof r);
        r.gfd = fd;
        if (isatty(fd)) {
            r.kind = CKF_TTY;              // inherited from the launcher pty down the restore fork
            r.flags = fcntl(fd, F_GETFD);  // preserve FD_CLOEXEC (bash's job-control fd-255 dup is cloexec)
        } else if (S_ISREG(st.st_mode)) {
            const char *p = (g_fdpath[fd][0]) ? g_fdpath[fd] : NULL;
            char fp[512];
            if (!p && fcntl(fd, F_GETPATH, fp) == 0) p = fp; // macOS host path fallback
            if (!p) {
                fprintf(stderr, "[ckpt] refuse: fd %d is a regular file with no recoverable path\n", fd);
                return -1;
            }
            r.kind = CKF_FILE;
            r.flags = fcntl(fd, F_GETFL);
            off_t off = lseek(fd, 0, SEEK_CUR);
            r.offset = (off == (off_t)-1) ? 0 : (int64_t)off;
            snprintf(r.path, sizeof r.path, "%s", p);
        } else {
            const char *ty = ckpt_guest_kernel_fd(fd);
            if (ty) {
                fprintf(stderr,
                        "[ckpt] refuse: guest fd %d is a %s -- pathless kernel-object fds are P3 (not yet "
                        "supported)\n",
                        fd, ty);
                return -1;
            }
            continue; // engine-internal descriptor -> not part of the checkpoint
        }
        recs[n++] = r;
    }
    *out_n = n;
    return 0;
}

static int ckpt_region_prot(uint64_t addr, uint64_t glen) {
    int p = anon_prot_if_contained(addr, glen ? glen : 1);
    return p >= 0 ? p : (PROT_READ | PROT_WRITE);
}

// Sparse-dump every tracked guest mapping (image/interp/heap/stack/anon/file mmap). Non-zero HOST pages only.
static int ckpt_dump_pages(FILE *f, size_t pagesz, uint64_t *out_n) {
    uint64_t nreg = 0;
    static uint8_t zero[65536];
    for (int i = 0; i < g_ngmap; i++) {
        uint64_t addr = g_gmap[i].addr, len = g_gmap[i].len, glen = g_gmap[i].glen;
        if (!addr || !len) continue;
        struct ckpt_region reg;
        memset(&reg, 0, sizeof reg);
        reg.addr = addr;
        reg.len = len;
        reg.glen = glen;
        reg.prot = ckpt_region_prot(addr, glen);
        reg.is_gna = gna_hit(addr, 1);
        uint64_t npages = 0;
        for (uint64_t off = 0; off < len; off += pagesz) {
            uint64_t va = addr + off;
            size_t n = (len - off < pagesz) ? (size_t)(len - off) : pagesz;
            if (!host_range_mapped((uintptr_t)va, n)) continue;
            if (memcmp((void *)va, zero, n) != 0) npages++;
        }
        reg.npages = npages;
        if (ckpt_wr_all(f, &reg, sizeof reg) != 0) return -1;
        for (uint64_t off = 0; off < len; off += pagesz) {
            uint64_t va = addr + off;
            size_t n = (len - off < pagesz) ? (size_t)(len - off) : pagesz;
            if (!host_range_mapped((uintptr_t)va, n)) continue;
            if (memcmp((void *)va, zero, n) == 0) continue;
            if (ckpt_wr_all(f, &va, sizeof va) != 0) return -1;
            if (ckpt_wr_all(f, (void *)va, n) != 0) return -1;
        }
        nreg++;
    }
    *out_n = nreg;
    return 0;
}

// This process's guest identity (pid / parent / group / session), mapped from host ids to guest space (the
// container init's real host pid/group/session all read back as 1). getppid()==g_init_hostpid means "child
// of init"; a host pgid/sid equal to g_init_hostpid is the container's own group/session (guest 1).
static void ckpt_self_identity(struct ckpt_meta *m) {
    int self = container_pid();
    m->self_gpid = self;
    if (self == 1) {
        m->ppid_gpid = 0;
    } else {
        int pp = getppid();
        m->ppid_gpid = (g_init_hostpid && pp == g_init_hostpid) ? 1 : pp;
    }
    int pg = getpgid(0);
    m->pgid_gpid = (g_init_hostpid && pg == g_init_hostpid) ? 1 : pg;
    int sd = getsid(0);
    m->sid_gpid = (g_init_hostpid && sd == g_init_hostpid) ? 1 : sd;
}

// Dump THIS process (RAM + cpu + fds) into `procdir` (temp dir + rename). Returns 0 on success, -1 on any
// failure or P3 refusal (nothing published on failure).
static int ckpt_dump_self(struct cpu *c, const char *procdir) {
    if (g_untrusted) {
        fprintf(stderr, "[ckpt] refuse: sentry/untrusted split is P3\n");
        return -1;
    }
    int nthreads = ckpt_live_threads();
    if (nthreads > 1) {
        fprintf(stderr, "[ckpt] refuse proc %d: %d live guest threads -- multi-threaded checkpoint is P3\n",
                container_pid(), nthreads);
        return -1;
    }

    static struct ckpt_fd fdrecs[1024];
    int nfd = 0;
    if (ckpt_scan_fds(fdrecs, 1024, &nfd) != 0) return -1; // P3 refusal already reported

    // Ensure the base checkpoint dir exists (a peer may reach here before the coordinator's mkdir; idempotent).
    mkdir(g_ckpt_dir, 0700);
    char tmp[1280];
    snprintf(tmp, sizeof tmp, "%s.tmp.%d", procdir, (int)getpid());
    ckpt_rmrf(tmp);
    if (mkdir(tmp, 0700) != 0) {
        fprintf(stderr, "[ckpt] mkdir %s: %s\n", tmp, strerror(errno));
        return -1;
    }
    char pf[1400];
    FILE *fp = NULL, *fc = NULL, *ff = NULL, *fm = NULL;
    int ok = 0;
    size_t pagesz = (size_t)getpagesize();

    struct ckpt_meta m;
    memset(&m, 0, sizeof m);
    m.magic = CKPT_MAGIC;
    m.version = CKPT_VERSION;
    m.arch = CKPT_ARCH_AARCH64;
    m.engine_id = pcache_engine_id();
    m.cpu_sz = sizeof(struct cpu);
    m.pagesz = pagesz;
    m.n_threads = 1;
    m.brk_lo = brk_lo;
    m.brk_cur = brk_cur;
    m.brk_hi = brk_hi;
    m.nonpie_lo = g_nonpie_lo;
    m.nonpie_hi = g_nonpie_hi;
    m.nonpie_bias = g_nonpie_bias;
    m.stack_lo = g_stack_lo;
    m.stack_hi = g_stack_hi;
    m.n_fds = (uint64_t)nfd;
    ckpt_self_identity(&m);
    snprintf(m.exe_path, sizeof m.exe_path, "%s", g_exe_path ? g_exe_path : "");
    for (int s = 0; s < 65; s++) { // capture this process's guest signal dispositions (restored on thaw)
        m.sig_handler[s] = g_sigact[s].handler;
        m.sig_flags[s] = g_sigact[s].flags;
        m.sig_mask[s] = g_sigact[s].mask;
    }

    snprintf(pf, sizeof pf, "%s/pages", tmp);
    fp = fopen(pf, "wb");
    if (!fp || ckpt_dump_pages(fp, pagesz, &m.n_regions) != 0) goto done;

    snprintf(pf, sizeof pf, "%s/cpu", tmp);
    fc = fopen(pf, "wb");
    if (!fc || ckpt_wr_all(fc, c, sizeof *c) != 0) goto done;

    snprintf(pf, sizeof pf, "%s/fds", tmp);
    ff = fopen(pf, "wb");
    if (!ff) goto done;
    for (int i = 0; i < nfd; i++)
        if (ckpt_wr_all(ff, &fdrecs[i], sizeof fdrecs[i]) != 0) goto done;

    snprintf(pf, sizeof pf, "%s/meta", tmp); // meta written LAST (carries the section counts)
    fm = fopen(pf, "wb");
    if (!fm || ckpt_wr_all(fm, &m, sizeof m) != 0) goto done;
    ok = 1;

done:
    if (fp) fclose(fp);
    if (fc) fclose(fc);
    if (ff) fclose(ff);
    if (fm) fclose(fm);
    if (!ok) {
        ckpt_rmrf(tmp);
        return -1;
    }
    ckpt_rmrf(procdir);
    if (rename(tmp, procdir) != 0) {
        fprintf(stderr, "[ckpt] rename %s -> %s: %s\n", tmp, procdir, strerror(errno));
        ckpt_rmrf(tmp);
        return -1;
    }
    return 0;
}

// Enumerate the container's whole process tree = every ENGINE process in the init's session. dd runs each
// guest process as a real host process and the launcher setsid()s the container init, so every guest process
// (even a fork-without-exec bash subshell, even one orphaned to launchd after its parent exited) keeps the
// init's session id. The pid registry is unreliable here (a short-lived fork child inherits + unlinks the
// parent's registry entry on exit), so we scan the session table directly and filter to processes running
// OUR OWN executable -- excluding the launcher and any unrelated session member. Fills `pids` (peers only,
// self excluded), returns the count.
static int ckpt_enum_tree(int *pids, int cap) {
    char selfpath[PROC_PIDPATHINFO_MAXSIZE];
    if (proc_pidpath(getpid(), selfpath, sizeof selfpath) <= 0) return 0;
    int mysid = getsid(0);
    int mib[3] = {CTL_KERN, KERN_PROC, KERN_PROC_ALL};
    size_t len = 0;
    if (sysctl(mib, 3, NULL, &len, NULL, 0) != 0 || len == 0) return 0;
    len += 16 * sizeof(struct kinfo_proc); // slack for processes spawned between the sizing and fetch calls
    struct kinfo_proc *kp = (struct kinfo_proc *)malloc(len);
    if (!kp) return 0;
    int n = 0;
    if (sysctl(mib, 3, kp, &len, NULL, 0) == 0) {
        int cnt = (int)(len / sizeof(struct kinfo_proc));
        for (int i = 0; i < cnt && n < cap; i++) {
            int pid = kp[i].kp_proc.p_pid;
            if (pid <= 0 || pid == (int)getpid()) continue;
            if (getsid(pid) != mysid) continue; // scope to OUR container's session (survives orphaning)
            char pp[PROC_PIDPATHINFO_MAXSIZE];
            if (proc_pidpath(pid, pp, sizeof pp) <= 0) continue;
            if (strcmp(pp, selfpath) != 0) continue; // only our own engine processes are container peers
            pids[n++] = pid;
        }
    }
    free(kp);
    return n;
}

// The container INIT (guest pid 1) coordinates a whole-tree checkpoint at its safepoint: freeze + dump every
// peer, then itself, then publish the MANIFEST. Never returns (_exit frees init's RAM).
static void ckpt_coordinate_and_exit(struct cpu *c) {
    char base[1024];
    snprintf(base, sizeof base, "%s", g_ckpt_dir);
    // The requester prepared a fresh, empty base dir BEFORE advancing the trigger (so peers that see the
    // shared generation independently can drop their proc.<gpid> straight in). We must NOT rm it here — a
    // peer may already have dumped into it. Just ensure it exists (idempotent) and proceed.
    mkdir(base, 0700);

    static int foll[512];
    int nfoll = ckpt_enum_tree(foll, 512);
    fprintf(stderr, "[ckpt] coordinator pid=%d found %d peer(s)\n", getpid(), nfoll);

    // Freeze + dump every peer: the shared trigger generation is already advanced (the requester bumped it),
    // so KICK each peer with the guest-proof THREAD_INT_SIG to bounce it out of a blocked syscall / chained
    // in-cache loop to its safepoint, where ckpt_poll sees the new generation and dumps proc.<gpid> + _exit()s.
    for (int i = 0; i < nfoll; i++) kill(foll[i], CKPT_KICK_SIG);
    for (int i = 0; i < nfoll; i++) {
        char pd[1200];
        snprintf(pd, sizeof pd, "%s/proc.%d", base, foll[i]);
        int done = 0;
        for (int t = 0; t < 500 && !done; t++) { // up to ~5s per peer
            if (access(pd, F_OK) == 0) {
                done = 1;
                break;
            }
            int st;
            while (waitpid(-1, &st, WNOHANG) > 0) {} // reap so a peer zombie doesn't linger
            usleep(10000);
        }
        if (!done) fprintf(stderr, "[ckpt] warning: peer %d did not checkpoint in time\n", foll[i]);
    }

    // Dump ourselves (the init) last.
    char selfpd[1200];
    snprintf(selfpd, sizeof selfpd, "%s/proc.1", base);
    if (ckpt_dump_self(c, selfpd) != 0) {
        fprintf(stderr, "[ckpt] init dump FAILED -- checkpoint incomplete\n");
        _exit(70);
    }

    // Publish the MANIFEST last: its presence == a complete, restorable checkpoint.
    int nproc = 0;
    DIR *bd = opendir(base);
    if (bd) {
        struct dirent *e;
        while ((e = readdir(bd)))
            if (!strncmp(e->d_name, "proc.", 5) && !strstr(e->d_name, ".tmp.")) nproc++;
        closedir(bd);
    }
    struct ckpt_manifest man;
    memset(&man, 0, sizeof man);
    man.magic = CKPT_MANIFEST_MAGIC;
    man.version = CKPT_VERSION;
    man.arch = CKPT_ARCH_AARCH64;
    man.n_procs = (uint64_t)nproc;
    man.root_gpid = 1;
    // Record which group owns the controlling terminal's foreground (in guest terms). The init is the tty's
    // session leader here, so tcgetpgrp reads the real fg host pgid; child job groups pass through untranslated
    // (guest pgid == host pgid), only the init's own group folds to guest pgid 1.
    {
        int tf = isatty(0) ? 0 : isatty(1) ? 1 : isatty(2) ? 2 : -1;
        int fgh = (tf >= 0) ? tcgetpgrp(tf) : -1;
        man.fg_pgid_gpid = (fgh <= 0) ? 0 : (g_init_hostpid && fgh == g_init_hostpid) ? 1 : fgh;
    }
    char mp[1200];
    snprintf(mp, sizeof mp, "%s/MANIFEST", base);
    FILE *mf = fopen(mp, "wb");
    if (mf) {
        ckpt_wr_all(mf, &man, sizeof man);
        fclose(mf);
    }
    fprintf(stderr, "[ckpt] workspace checkpoint OK: %d process(es) -> %s\n", nproc, base);
    int st;
    while (waitpid(-1, &st, WNOHANG) > 0) {} // final reap
    _exit(0);
}

// ================================= RESTORE =================================

static int ckpt_read_manifest(const char *dir, struct ckpt_manifest *man) {
    char pf[1200];
    snprintf(pf, sizeof pf, "%s/MANIFEST", dir);
    FILE *f = fopen(pf, "rb");
    if (!f) {
        fprintf(stderr, "[restore] %s has no MANIFEST (not a complete checkpoint)\n", dir);
        return -1;
    }
    int r = ckpt_rd_all(f, man, sizeof *man);
    fclose(f);
    if (r != 0 || man->magic != CKPT_MANIFEST_MAGIC) {
        fprintf(stderr, "[restore] %s: bad manifest magic\n", dir);
        return -1;
    }
    if (man->version != CKPT_VERSION || man->arch != CKPT_ARCH_AARCH64) {
        fprintf(stderr, "[restore] manifest version/arch mismatch\n");
        return -1;
    }
    return 0;
}

static int ckpt_read_meta_dir(const char *procdir, struct ckpt_meta *m) {
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/meta", procdir);
    FILE *f = fopen(pf, "rb");
    if (!f) {
        fprintf(stderr, "[restore] open %s: %s\n", pf, strerror(errno));
        return -1;
    }
    int r = ckpt_rd_all(f, m, sizeof *m);
    fclose(f);
    if (r != 0 || m->magic != CKPT_MAGIC) {
        fprintf(stderr, "[restore] %s is not a checkpoint (bad magic/short read)\n", procdir);
        return -1;
    }
    if (m->version != CKPT_VERSION || m->arch != CKPT_ARCH_AARCH64) {
        fprintf(stderr, "[restore] version/arch mismatch (file v%llu arch %llu)\n",
                (unsigned long long)m->version, (unsigned long long)m->arch);
        return -1;
    }
    if (m->cpu_sz != sizeof(struct cpu)) {
        fprintf(stderr, "[restore] cpu-struct size mismatch (file %llu, engine %zu)\n",
                (unsigned long long)m->cpu_sz, sizeof(struct cpu));
        return -1;
    }
    if (m->n_threads != 1) {
        fprintf(stderr, "[restore] %llu threads in checkpoint -- multi-threaded restore is P3\n",
                (unsigned long long)m->n_threads);
        return -1;
    }
    return 0;
}

// Rebuild this process's guest memory (MAP_FIXED) + the mapping side-registries from `procdir`. For the init
// this runs BEFORE engine init (so MAP_FIXED lands on free VAs); a re-forked child calls gmap_reset_all() +
// clears the anon/gna counters FIRST (dropping the COW-inherited init mappings) so its own RAM lands clean.
static int ckpt_restore_mem_dir(const char *procdir, const struct ckpt_meta *m) {
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/pages", procdir);
    FILE *f = fopen(pf, "rb");
    if (!f) {
        fprintf(stderr, "[restore] open %s: %s\n", pf, strerror(errno));
        return -1;
    }
    uint64_t mapped_a[GMAP_N], mapped_e[GMAP_N];
    int nmapped = 0;
    for (uint64_t i = 0; i < m->n_regions; i++) {
        struct ckpt_region reg;
        if (ckpt_rd_all(f, &reg, sizeof reg) != 0) {
            fclose(f);
            return -1;
        }
        uint64_t a = reg.addr, e = reg.addr + reg.len;
        int contained = 0;
        for (int j = 0; j < nmapped; j++)
            if (mapped_a[j] <= a && e <= mapped_e[j]) {
                contained = 1;
                break;
            }
        if (!contained) {
            void *r = mmap((void *)a, (size_t)reg.len, PROT_READ | PROT_WRITE,
                           MAP_FIXED | MAP_ANON | MAP_PRIVATE, -1, 0);
            if (r == MAP_FAILED || (uint64_t)(uintptr_t)r != a) {
                fprintf(stderr, "[restore] cannot map guest region %llx+%llx: %s\n", (unsigned long long)a,
                        (unsigned long long)reg.len, strerror(errno));
                fclose(f);
                return -1;
            }
            if (nmapped < GMAP_N) {
                mapped_a[nmapped] = a;
                mapped_e[nmapped] = e;
                nmapped++;
            }
        }
        for (uint64_t p = 0; p < reg.npages; p++) {
            uint64_t va;
            if (ckpt_rd_all(f, &va, sizeof va) != 0) {
                fclose(f);
                return -1;
            }
            size_t n =
                (va - reg.addr + m->pagesz > reg.len) ? (size_t)(reg.len - (va - reg.addr)) : (size_t)m->pagesz;
            if (ckpt_rd_all(f, (void *)va, n) != 0) {
                fclose(f);
                return -1;
            }
        }
        ckpt_place_bump_past(reg.addr + reg.len); // keep the high-arena cursor above every restored region
        gmap_add(reg.addr, reg.len);
        gmap_set_glen(reg.addr, reg.glen);
        if (reg.is_gna)
            gna_add(reg.addr & ~(uint64_t)0xfff, (reg.addr + reg.glen + 0xfff) & ~(uint64_t)0xfff);
        else
            anon_track(reg.addr, reg.len, reg.prot);
    }
    fclose(f);
    brk_lo = m->brk_lo;
    brk_cur = m->brk_cur;
    brk_hi = m->brk_hi;
    g_nonpie_lo = m->nonpie_lo;
    g_nonpie_hi = m->nonpie_hi;
    g_nonpie_bias = m->nonpie_bias;
    g_stack_lo = m->stack_lo;
    g_stack_hi = m->stack_hi;
    return 0;
}

// Reopen this process's own path-backed fds. TTY fds are NOT reopened here -- they are inherited down the
// restore fork from the launcher's pty (init got 0/1/2 from the launcher; each child inherits them).
static int ckpt_restore_fds_dir(const char *procdir) {
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/fds", procdir);
    FILE *f = fopen(pf, "rb");
    if (!f) return 0;
    struct ckpt_fd r;
    while (ckpt_rd_all(f, &r, sizeof r) == 0) {
        if (r.kind == CKF_TTY) {
            // 0/1/2 are inherited from the launcher pty. But an interactive shell also keeps a HIGH-fd dup of
            // the controlling terminal for job control (bash uses fd 255); the launcher doesn't provide it, so
            // recreate it by duping the ctty onto that number -- else the shell's tcsetattr/tcgetattr on it
            // fails EBADF after restore ("tcsetattr: Bad file descriptor" when a foreground job finishes).
            if (r.gfd > 2) {
                int ct = isatty(0) ? 0 : isatty(1) ? 1 : isatty(2) ? 2 : -1;
                if (ct >= 0 && r.gfd != ct && dup2(ct, r.gfd) >= 0 && (r.flags & FD_CLOEXEC))
                    fcntl(r.gfd, F_SETFD, FD_CLOEXEC);
            }
            continue;
        }
        if (r.kind == CKF_FILE) {
            int flags = r.flags & ~(O_CREAT | O_EXCL | O_TRUNC);
            int hf = open(r.path, flags);
            if (hf < 0) {
                fprintf(stderr, "[restore] warning: cannot reopen fd %d (%s): %s\n", r.gfd, r.path,
                        strerror(errno));
                continue;
            }
            if (hf != r.gfd) {
                dup2(hf, r.gfd);
                close(hf);
            }
            if (r.offset > 0) lseek(r.gfd, (off_t)r.offset, SEEK_SET);
            if (r.gfd >= 0 && r.gfd < 1024) snprintf(g_fdpath[r.gfd], sizeof g_fdpath[r.gfd], "%s", r.path);
        }
    }
    fclose(f);
    return 0;
}

static int ckpt_restore_cpu_dir(const char *procdir, struct cpu *c) {
    char pf[1300];
    snprintf(pf, sizeof pf, "%s/cpu", procdir);
    FILE *f = fopen(pf, "rb");
    if (!f || ckpt_rd_all(f, c, sizeof *c) != 0) {
        if (f) fclose(f);
        fprintf(stderr, "[restore] cannot read cpu state\n");
        return -1;
    }
    fclose(f);
    // Zero host-transient fields (meaningful only WHILE a block runs; run_block re-populates them). The
    // architectural state (x[],sp,pc,tls,nzcv,v[],sigmask,tpending,alt_*,tid,ctid) + shadow stack are verbatim.
    memset(c->host_save, 0, sizeof c->host_save);
    memset(c->host_v, 0, sizeof c->host_v);
    c->host_sp = 0;
    c->reason = 0;
    c->ic_site = 0;
    c->irq = 0;
    c->exited = 0;
    c->redirect = 0;
    return 0;
}

// Re-install this process's guest signal DISPOSITIONS after a restore, from the table carried in `m`.
// g_sigact (the guest handler table) is engine C state, not guest RAM, so it is NOT in the page dump; a
// freshly re-launched/-forked restore process starts with an all-SIG_DFL table and DEFAULT host
// dispositions -- so a ^C (SIGINT) at a restored bash prompt hit the host default action (terminate) and
// KILLED the shell instead of running bash's interrupt handler (the "restore + Ctrl-C closes the tab" bug).
// This restores g_sigact and replays the exact host-side installation rt_sigaction(case 134) performs, so
// async signals route back through the engine's host_sigh and reach the guest handler.
static void ckpt_reinstall_sigacts(const struct ckpt_meta *m) {
    for (int s = 1; s <= 64; s++) {
        uint64_t h = m->sig_handler[s];
        g_sigact[s].handler = h;
        g_sigact[s].flags = m->sig_flags[s];
        g_sigact[s].mask = m->sig_mask[s];
        if (s == 9 || s == 19) continue; // SIGKILL/SIGSTOP: unmaskable, no host disposition to forward
        // SIGSEGV(11)/SIGBUS(7) keep the engine's permanent hardware-fault guard -- never overwrite it with a
        // plain disposition (that is exactly what rt_sigaction refuses to do). Only ILL/TRAP/FPE(4/5/8), whose
        // POSIX handler only ever sees an EXTERNAL kill, and the async signals forward to the host here.
        if (s == 7 || s == 11) continue;
        int ms = sig_l2m(s);
        if (h == 0) {
            signal(ms, SIG_DFL);
        } else if (h == 1) {
            signal(ms, SIG_IGN);
        } else {
            struct sigaction sa;
            memset(&sa, 0, sizeof sa);
            sa.sa_sigaction = (s == 4 || s == 5 || s == 8) ? host_sigh_sync : host_sigh_si;
            sa.sa_flags = SA_SIGINFO;
            sigfillset(&sa.sa_mask);
            sigaction(ms, &sa, NULL);
        }
    }
}

// The process table read from the checkpoint (one entry per proc.<gpid>/meta), used to rebuild the tree.
struct ckpt_proc {
    int gpid, ppid, pgid, sid;
};
static struct ckpt_proc g_rprocs[512];
static int g_nrprocs;

static int ckpt_scan_procs(const char *base) {
    g_nrprocs = 0;
    DIR *d = opendir(base);
    if (!d) return -1;
    struct dirent *e;
    while ((e = readdir(d)) && g_nrprocs < 512) {
        if (strncmp(e->d_name, "proc.", 5)) continue;
        if (strstr(e->d_name, ".tmp.")) continue;
        int gpid = atoi(e->d_name + 5);
        if (gpid <= 0) continue;
        char pd[1200];
        snprintf(pd, sizeof pd, "%s/%s", base, e->d_name);
        struct ckpt_meta m;
        if (ckpt_read_meta_dir(pd, &m) != 0) continue;
        g_rprocs[g_nrprocs].gpid = gpid;
        g_rprocs[g_nrprocs].ppid = m.ppid_gpid;
        g_rprocs[g_nrprocs].pgid = m.pgid_gpid;
        g_rprocs[g_nrprocs].sid = m.sid_gpid;
        g_nrprocs++;
    }
    closedir(d);
    return g_nrprocs > 0 ? 0 : -1;
}

// The guest group (guest pgid; 1 == init) that owned the controlling terminal's foreground at checkpoint,
// carried from the manifest so the matching re-forked leader can reclaim the tty. Inherited across the
// restore forks (set in the init before it forks the tree).
static int g_ckpt_fg_gpid = 0;

// Make THIS process's group the controlling terminal's foreground group. Called by the process that led the
// foreground job's group at checkpoint, right AFTER it re-creates that group (ckpt_restore_pgrp), so the pgid
// argument names a group that already exists. SIGTTOU is blocked so a background caller isn't stopped by the
// handoff. Best-effort -- a failure just leaves the (safe) inherited foreground group.
static void ckpt_claim_tty_fg(void) {
    int tf = isatty(0) ? 0 : isatty(1) ? 1 : isatty(2) ? 2 : -1;
    if (tf < 0) return;
    sigset_t sv, bl;
    sigemptyset(&bl);
    sigaddset(&bl, SIGTTOU);
    sigprocmask(SIG_BLOCK, &bl, &sv);
    (void)tcsetpgrp(tf, getpgrp());
    sigprocmask(SIG_SETMASK, &sv, NULL);
}

// Best-effort reconstruction of this process's group/session relative to the LIVE (re-forked) tree. A
// process that led its own group/session re-creates it; otherwise it joins its (already-inherited) parent
// group. Errors are ignored (the process is at worst left in its inherited group -- never fatal).
static void ckpt_restore_pgrp(int gpid, int pgid_gpid, int sid_gpid) {
    if (sid_gpid == gpid && getsid(0) != getpid()) setsid(); // was a session leader
    if (pgid_gpid == gpid) {
        setpgid(0, 0); // was its own group leader
    } else if (pgid_gpid > 0) {
        int leader = (pgid_gpid == 1 && g_init_hostpid) ? g_init_hostpid : pidmap_to_live(pgid_gpid);
        setpgid(0, leader); // join the (usually already-inherited) parent group
    }
}

static void ckpt_restore_proc_run(const char *base, int gpid); // fwd

// Re-fork every child of `gpid` (per the checkpoint ppid table); each child restores its own subtree and
// resumes. Records the checkpoint-gpid -> live-hostpid mapping so this process's guest pids resolve.
static void ckpt_fork_children(const char *base, int gpid) {
    for (int i = 0; i < g_nrprocs; i++) {
        if (g_rprocs[i].ppid != gpid || g_rprocs[i].gpid == gpid) continue;
        int cg = g_rprocs[i].gpid;
        pid_t p = fork();
        if (p == 0) {
            ckpt_restore_proc_run(base, cg); // never returns
            _exit(0);
        } else if (p > 0) {
            pidmap_add(cg, (int)p);
        } else {
            fprintf(stderr, "[restore] fork for gpid %d failed: %s\n", cg, strerror(errno));
        }
    }
}

// Restore a re-forked CHILD process (runs in the fresh fork; the engine is already inited, inherited from the
// parent) and resume it. Never returns.
static void ckpt_restore_proc_run(const char *base, int gpid) {
    char pd[1200];
    snprintf(pd, sizeof pd, "%s/proc.%d", base, gpid);
    struct ckpt_meta m;
    if (ckpt_read_meta_dir(pd, &m) != 0) _exit(70);

    // drop the COW-inherited parent guest memory + registries, then load our own
    gmap_reset_all();
    g_nanonmap = 0;
    g_ngna = 0;
    if (ckpt_restore_mem_dir(pd, &m) != 0) _exit(70);

    // adopt our restored identity BEFORE any pid-reporting syscall or /proc publish
    g_self_gpid = m.self_gpid;
    g_self_gppid = m.ppid_gpid;

    struct cpu c;
    if (ckpt_restore_cpu_dir(pd, &c) != 0) _exit(70);
    fork_child_hooks(&c); // shared after-fork engine reset (cache re-alias, kqueue rebuild, lock/threg/Mach)
    ckpt_reinstall_sigacts(&m); // restore guest signal dispositions (AFTER the fork hooks reset host state)

    ckpt_restore_fds_dir(pd);
    ckpt_restore_pgrp(gpid, m.pgid_gpid, m.sid_gpid);
    if (g_ckpt_fg_gpid == gpid) ckpt_claim_tty_fg(); // this process led the tty's foreground job -> reclaim it

    static char exe[512];
    snprintf(exe, sizeof exe, "%s", m.exe_path);
    if (exe[0]) g_exe_path = exe;
    char *pubargv[2] = {(char *)(exe[0] ? exe : "guest"), NULL};
    proc_reg_publish(g_exe_path, 1, pubargv);

    ckpt_fork_children(base, gpid); // re-fork our own children before we resume (so a wait finds them)
    run_guest(&c);
    _exit(c.exit_code);
}

// Full restore driver: rebuild the whole tree from `dir` and resume it. The INIT (gpid 1) restores its RAM
// FIRST (before engine init, so MAP_FIXED lands on free VAs), then re-forks the tree.
static int ckpt_restore_tree(const char *rootfs, const char *dir) {
    struct ckpt_manifest man;
    if (ckpt_read_manifest(dir, &man) != 0) return 2;
    if (ckpt_scan_procs(dir) != 0) {
        fprintf(stderr, "[restore] no processes found in %s\n", dir);
        return 2;
    }

    char ipd[1200];
    snprintf(ipd, sizeof ipd, "%s/proc.1", dir);
    struct ckpt_meta im;
    if (ckpt_read_meta_dir(ipd, &im) != 0) return 2;
    if (ckpt_restore_mem_dir(ipd, &im) != 0) return 2; // init RAM before any engine allocation

    container_init(rootfs); // sets g_init_hostpid = getpid() -> this process becomes guest pid 1
    int irc = engine_global_init();
    if (irc) return irc;

    static char exe[512];
    snprintf(exe, sizeof exe, "%s", im.exe_path);
    if (exe[0]) g_exe_path = exe;
    ckpt_restore_fds_dir(ipd);
    struct cpu c;
    if (ckpt_restore_cpu_dir(ipd, &c) != 0) return 70;
    ckpt_reinstall_sigacts(&im); // restore the init's guest signal dispositions (so ^C reaches bash's handler)
    char *pubargv[2] = {(char *)(exe[0] ? exe : "guest"), NULL};
    proc_reg_publish(g_exe_path, 1, pubargv);

    // Publish which guest group owned the tty foreground, so whichever re-forked process is that group's leader
    // claims the controlling terminal AFTER it re-creates its group (see ckpt_claim_tty_fg). Set before the fork
    // so every child inherits it. Without this the resumed tree's fg group defaults to the init's, and a tty
    // SIGINT hits the init instead of the foreground job -> the whole tree dies on ^C.
    g_ckpt_fg_gpid = man.fg_pgid_gpid;
    ckpt_fork_children(dir, 1); // rebuild the tree BEFORE init runs (empty block map -> no stale translation)
    if (g_ckpt_fg_gpid == 1) ckpt_claim_tty_fg(); // the init itself was foreground (idle prompt)

    run_guest(&c);
    return c.exit_code;
}
