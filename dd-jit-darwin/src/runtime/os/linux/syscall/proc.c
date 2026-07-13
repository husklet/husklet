// Extracted from service(): Process & scheduling -- clone/fork/execve/wait/exit, pid/uid/gid identity,
// prctl/futex/caps/sched/affinity. Returns 1 if nr was handled, 0 otherwise. Because its cases call
// service.c-local helpers (nonpie_p/cpu_online_mask/affinity_mask), it is #included after them, before
// service(). NOTE: execve sets c->redirect; svc_done() (the shared tail) skips errno xlate when redirect
// is set, so a redirect's already-Linux G_RET is never re-translated.

// Restore guest GPRs that a per-arch fork/vfork->clone normalization repurposed as clone arguments (the x86
// frontend defines this in legacy.c; other frontends issue clone directly and need no fixup -> no-op).
#ifndef G_FORK_PRESERVE
#define G_FORK_PRESERVE(c) ((void)0)
#endif

// execve env forwarding: serialize the guest's envp array into DD_GUEST_ENV (the "K=V\nK=V..." string
// build_stack reads when laying out the new process stack), so the guest's actual environment crosses the
// re-exec. A guest-initiated exec makes the guest's envp AUTHORITATIVE (like Linux): whatever the guest
// passes -- including an EMPTY set for envp==NULL -- is EXACTLY what the new program sees. We mark it with
// DD_GUEST_ENV_EXACT so build_stack injects NONE of the engine's fallback defaults (PATH/HOME/LANG/
// GLIBC_TUNABLES). Those defaults are only appropriate on the INITIAL container launch (the daemon never
// sets DD_GUEST_ENV_EXACT); a guest execve that curates its env -- or clears it -- must match Linux, where
// `execve(path, argv, NULL)` yields an empty environment and `execve(path,argv,["FOO=bar"])` yields exactly
// one entry. Each pointer may be a low non-PIE address, so rebase the array base and every element with
// nonpie_p(), exactly as the argv loop does. setenv() copies the buffer, so it survives the teardown.
static void exec_forward_env(uint64_t envp_guest) {
    if (!envp_guest) {
        // Linux: NULL envp -> the new program runs with an EMPTY environment. Publish an empty, authoritative
        // env (do NOT leak the container's stale/default DD_GUEST_ENV) and flag it EXACT so build_stack adds
        // no defaults -> the guest sees envc==0, byte-exact with the native oracle.
        setenv("DD_GUEST_ENV", "", 1);
        setenv("DD_GUEST_ENV_ESC", "1", 1);
        setenv("DD_GUEST_ENV_EXACT", "1", 1);
        return;
    }
    uint64_t *ev = (uint64_t *)nonpie_p(envp_guest);
    size_t cap = 4096, len = 0;
    char *buf = malloc(cap);
    if (!buf) return;
    buf[0] = 0;
    for (int i = 0; ev[i]; i++) {
        const char *e = (const char *)nonpie_p(ev[i]);
        size_t el = strlen(e);
        // '\n' is DD_GUEST_ENV's record separator, but Linux permits newline (and any non-NUL byte) INSIDE an
        // env value. Escape '\\'->"\\\\" and '\n'->"\\n" so a raw '\n' only ever marks a record boundary;
        // build_stack unescapes when DD_GUEST_ENV_ESC=1. Worst case each byte becomes 2 -> reserve 2*el+2.
        if (len + 2 * el + 2 > cap) {
            cap = (len + 2 * el + 2) * 2;
            char *nb = realloc(buf, cap);
            if (!nb) {
                free(buf);
                return;
            }
            buf = nb;
        }
        for (size_t j = 0; j < el; j++) {
            char ch = e[j];
            if (ch == '\\') {
                buf[len++] = '\\';
                buf[len++] = '\\';
            } else if (ch == '\n') {
                buf[len++] = '\\';
                buf[len++] = 'n';
            } else {
                buf[len++] = ch;
            }
        }
        buf[len++] = '\n'; // DD_GUEST_ENV record separator (build_stack splits on '\n')
        buf[len] = 0;
    }
    setenv("DD_GUEST_ENV", buf, 1);
    setenv("DD_GUEST_ENV_ESC", "1", 1);   // tell build_stack the records are escape-encoded
    setenv("DD_GUEST_ENV_EXACT", "1", 1); // guest-initiated exec: this env is authoritative, inject no defaults
    free(buf);
}

// Fill a guest `struct rlimit { rlim_cur; rlim_max; }` for {get,set}rlimit/prlimit64 (cases 163/261).
// Shared so both forms report identical limits. Most resources are unlimited, but a few MUST be finite or
// guests size data structures off them: RLIMIT_STACK(3) reports the conventional 8MB main-stack size, and
// RLIMIT_NOFILE(7) reports a finite fd cap (soft 20480 / hard 1048576, the docker container default) -- a
// guest like memcached does calloc(rlim_cur, sizeof(conn)), which overflows if the soft limit is RLIM_INFINITY.
static void svc_fill_rlimit(int resource, uint64_t *o) {
    // docker --ulimit override wins (g_ulimit, seeded from DD_ULIMITS in state.c): a guest that reads its
    // limits (memcached calloc's off RLIMIT_NOFILE, the JVM sizes threads off RLIMIT_NPROC) must see the
    // requested value, not the dd default. `set` gates each resource so unspecified ones keep the defaults.
    if (resource >= 0 && resource < DD_RLIM_MAX && g_ulimit[resource].set) {
        o[0] = g_ulimit[resource].cur;
        o[1] = g_ulimit[resource].max;
        return;
    }
    switch (resource) {
    case 3: // RLIMIT_STACK
        o[0] = 8ull << 20;
        o[1] = ~0ull;
        break;
    case 7: // RLIMIT_NOFILE -- docker container default (oracle: soft 20480, hard 1048576; was 1024/1048576)
        o[0] = 20480;
        o[1] = 1048576;
        break;
    case 4: // RLIMIT_CORE -- match the Linux/docker default: cores OFF via soft=0, hard unlimited. A guest that
            // wants cores (LTP, crash handlers) raises rlim_cur with setrlimit; that soft limit governs whether
            // wait4/waitid report WCOREDUMP. Reporting the old RLIM_INFINITY here made every crash look
            // core-enabled, diverging from a native run (which inherits the container/host soft=0).
        o[0] = 0;
        o[1] = ~0ull;
        break;
    default:
        o[0] = ~0ull; // RLIM_INFINITY
        o[1] = ~0ull;
        break;
    }
}

// sig_coredumps() and svc_core_rlimit_cur() are defined in os/linux/signal.c (included before this TU);
// wait4/waitid below use them to synthesize WCOREDUMP. (reconciled to a single definition.)

// Emulate the kernel's close-on-exec sweep. The JIT's execve re-loads the new image IN-PROCESS (no real
// host exec happens -- see case 221), so the kernel never closes FD_CLOEXEC descriptors for us. We must do
// it by hand, or a guest fd the caller opened O_CLOEXEC leaks into the new image. The classic failure this
// fixes: initdb forks `postgres --boot` and feeds it the bootstrap script over a pipe2(O_CLOEXEC); the
// child dup2()s the read end onto stdin and execs, expecting its inherited (CLOEXEC) copy of the WRITE end
// to vanish on exec. Without this sweep that copy survives, so the pipe still has a writer after initdb
// closes its end -> the child's read(stdin) never sees EOF and `running bootstrap script ...` hangs forever.
// Engine-private host fds (the rootfs/volume dir-fds, the timer kqueue, the signal self-pipe) are skipped:
// they back the runtime itself and must survive the emulated exec; closing them would leave dangling fd
// numbers the new guest could reuse, corrupting timer/signal delivery and path confinement.
static int exec_fd_is_engine(int fd) {
    if (fd < 0) return 1;
    if (eventfd_peer_is_engine_fd(fd)) return 1;
    if (sfd_wr_is(fd)) return 1; // signalfd write ends are engine-private (read ends are ordinary guest fds)
    if (fd == g_root_fd || fd == g_gtimer_kq) return 1;
    for (int i = 0; i < g_nvols; i++)
        if (fd == g_vols[i].fd) return 1;
    return 0;
}

// Close the CLOEXEC guest fds among a bounded [0,maxfd) range (proc_pidinfo fallback path only).
// The caller passes the REAL descriptor-table size (getdtablesize() = the current soft RLIMIT_NOFILE),
// so do NOT clamp it DOWN -- that would leave CLOEXEC fds above the cap open across exec. The ceiling
// only guards a garbage/negative getdtablesize() return from spinning an absurd loop.
static void exec_close_cloexec_scan(int maxfd) {
    if (maxfd < 0 || maxfd > (1 << 20)) maxfd = 4096;
    for (int fd = 0; fd < maxfd; fd++) {
        if (exec_fd_is_engine(fd)) continue;
        int fl = fcntl(fd, F_GETFD);
        if (fl >= 0 && (fl & FD_CLOEXEC)) {
            fd_reset_emul(fd); // drop dd's emulation tables for this fd so a reused number isn't misrouted
            close(fd);
        }
    }
}

#include <libproc.h> // proc_pidinfo(PROC_PIDLISTFDS): enumerate only the OPEN fds (see below)

static void exec_close_cloexec(void) {
    // Sweep only the fds that are actually OPEN, not the whole descriptor table. The daemon raises the
    // soft fd limit very high (getdtablesize() ~= 180K), so the old `for (fd=0; fd<getdtablesize())` scan
    // issued ~180K fcntl(F_GETFD) syscalls -- ~21ms -- on EVERY execve. That single loop dominated the cost
    // of an exec and made process-spawn-heavy guests (make/configure/npm/go/pip fork+exec hundreds to
    // thousands of children) appear to hang: 21ms x thousands of execs = seconds of pure descriptor
    // scanning. PROC_PIDLISTFDS returns just the live descriptors (a couple dozen), so the sweep becomes
    // O(open fds). The real close-on-exec semantics are unchanged: every open non-engine CLOEXEC fd is
    // still closed. Fall back to a bounded linear scan only if proc_pidinfo is unavailable.
    int need = proc_pidinfo(getpid(), PROC_PIDLISTFDS, 0, NULL, 0);
    if (need <= 0) {
        exec_close_cloexec_scan(getdtablesize());
        return;
    }
    // Over-allocate a little: fds can be opened between the sizing call and the listing call.
    int cap = need + 32 * (int)sizeof(struct proc_fdinfo);
    struct proc_fdinfo *fds = malloc((size_t)cap);
    if (!fds) {
        exec_close_cloexec_scan(getdtablesize());
        return;
    }
    int got = proc_pidinfo(getpid(), PROC_PIDLISTFDS, 0, fds, cap);
    if (got <= 0) {
        free(fds);
        exec_close_cloexec_scan(getdtablesize());
        return;
    }
    int n = got / (int)sizeof(struct proc_fdinfo);
    for (int i = 0; i < n; i++) {
        int fd = fds[i].proc_fd;
        if (exec_fd_is_engine(fd)) continue;
        int fl = fcntl(fd, F_GETFD);
        if (fl >= 0 && (fl & FD_CLOEXEC)) {
            fd_reset_emul(fd); // drop dd's emulation tables for this fd so a reused number isn't misrouted
            close(fd);
        }
    }
    free(fds);
}

// ---- fork child-side engine hooks (shared by clone/case-220 and clone3/case-435) -----------------
// Everything the CHILD must reset before it re-enters guest code. Factored so the two fork sites can
// never drift (clone3 was missing the W^X re-assert and the DIR*-cache drop), and instrumented:
// fork-cost histogram (DD_FORKPROF=1, read once) prints one stderr line per fork with per-hook wall-ns
// deltas so a regression in the fork path is measurable, at zero cost when unset.
static int forkprof_on(void) {
    static int on = -1;
    if (on < 0) {
        const char *e = getenv("DD_FORKPROF");
        on = (e && e[0] == '1') ? 1 : 0;
    }
    return on;
}

static void fork_child_hooks(struct cpu *c) {
    uint64_t t[6] = {0};
    int fp = forkprof_on();
    if (fp) t[0] = now_ns();
    // Re-assert MAP_JIT execute mode: the per-thread W^X/APRR state isn't reliable across fork(),
    // so the child's first run_block can instruction-abort fetching from the (non-executable) code
    // cache -> the intermittent fork+exec SIGBUS. pthread_jit_write_protect_np(1) = RX (executable).
    // (No-op under the dual map, which never toggles W^X.)
    pthread_jit_write_protect_np(1);
    install_host_sigaltstack(); // the altstack registration doesn't survive fork on Apple Silicon --
                                // re-arm it (COW-inherited region) so the child can still take a stack-overflow
                                // guard fault (host SP == guest SP) instead of double-faulting into SIGILL.
    jit_after_fork();           // dual map: re-alias RX from the child's COW RW pages at the same VA (~1us; keeps
                                // every inherited translation valid) -- or, threaded parent, rebuild a fresh cache
#ifdef PCACHE_FORK_HOOK
    PCACHE_FORK_HOOK; // drop inherited reloc records + bar child saves (an execve re-keys + unbars)
#endif
    G_SHADOW_RESET(c); // §B: child's pre-fork host_rets crossed run_block -> drop, use IBTC
    // Only when jit_after_fork REBUILT the cache at a fresh VA (threaded parent) is every cached body
    // pointer stale: it zeroed the shared g_map + g_ibtc, but the x86-only 2-way g_xibtc it cannot see
    // must ALSO be dropped -- else the child's first indirect branch resolves a stale body into the freed
    // parent RX alias -> SIGSEGV (the same class the execve path documents below). On the preserved-arena
    // path (single-threaded parent, or the MAP_JIT fallback) the cache VA and content are unchanged,
    // so the inherited g_xibtc stays valid and is kept warm.
    if (g_dualmap && !g_fork_preserved) G_SHADOW_CLEAR(c);
    if (fp) t[1] = now_ns();
    rc_reset(); // S2: invalidate the inherited (COW) path/metadata caches so the child can never serve
                // an entry the parent populated before the FS diverged (generation bump; see fscache.c)
    if (fp) t[2] = now_ns();
    g_ndirs = 0;                 // the getdents DIR* cache is the PARENT's -- closedir'ing inherited handles
                                 // (on the child's close) crashes; drop it so the child re-fdopendir's fresh
    kqueue_rebuild_after_fork(); // macOS kqueue() fds (epoll/timerfd/inotify) don't survive fork ->
                                 // rebuild them so the child doesn't EBADF on its inherited event fds
                                 // (also reinits g_ep_mtx, inherited-locked if a peer forked mid-epoll)
    if (fp) t[3] = now_ns();
    thread_after_fork();    // reset process-private thread/futex locks a dead peer may have held at fork
    seq_wrote_after_fork(); // a forked child has WRITTEN to none of its inherited SEQPACKET/pipe ends yet:
                            // clear the "wrote" table so a bystander child closing an inherited Mojo channel
                            // end injects no spurious peer-EOF into another process's live channel (see netns.c)
    sysv_after_fork();      // reset the SysV-shm lock (same fork-unsafe-mutex class)
    eventfd_after_fork();   // reset the eventfd counter+pipe lock (fork-unsafe-mutex class)
    ts_after_fork();        // drop the inherited task-state slot cache so the child re-claims its own
    poslk_after_fork();     // re-cache pid; child inherits NONE of the parent's fcntl record locks
    proc_reg_after_fork();  // publish the fork child in /proc and stop it inheriting the parent's registry path
    acct_after_fork();      // claim this child's OWN cgroup accounting slot (new host pid, one task)
    wipefork_apply_child(); // MADV_WIPEONFORK: zero-fill the ranges the guest marked wipe-on-fork
    mlk_reset();            // mlock(2): memory locks are NOT inherited across fork -> child starts unlocked
    dd_gpu_after_fork();    // GPU rung 2: this child can no longer create/map an IOSurface (fork-unsafe) —
                            // it may only reuse the pre-fork, VM_INHERIT_SHARE'd IOSurface pool it inherited
    if (fp) t[4] = now_ns();
#ifdef DD_HAS_MACH_EXC
    // The CRASHDBG Mach exception port + its receiver thread do NOT survive fork, so a crash in the
    // child silently dies. Clear the inherited task exception port so a fault falls through to the
    // POSIX diag_crash handler (which IS inherited) and reports fault=/pc=.
    if (getenv("CRASHDBG"))
        task_set_exception_ports(mach_task_self(), EXC_MASK_BAD_ACCESS | EXC_MASK_BAD_INSTRUCTION, MACH_PORT_NULL,
                                 EXCEPTION_DEFAULT, 0);
#endif
    if (fp) {
        t[5] = now_ns();
        fprintf(stderr, "[forkprof] child jit=%llu rc=%llu kq=%llu locks=%llu total=%llu ns preserved=%d rw=%p rx=%p\n",
                (unsigned long long)(t[1] - t[0]), (unsigned long long)(t[2] - t[1]), (unsigned long long)(t[3] - t[2]),
                (unsigned long long)(t[4] - t[3]), (unsigned long long)(t[5] - t[0]), g_fork_preserved, (void *)g_cache,
                J_RX(g_cache));
    }
}

// ---- runtime credential overlay (USER ns) -------------------------------------------------------
// The credential overlay state + accessors (g_ruid/euid/suid, g_rgid/egid/sgid, cred_init/cred_euid/
// cred_egid/uid_permitted/gid_permitted) and the new-file ownership stamp (g_fs*_ovr, newfile_*)
// are defined in os/linux/container/state.c -- BEFORE both fs.c (create sites) and this file in the
// unity TU -- so the fs.c create paths and these set*id handlers share one view. See there for the
// apt `_apt` / gosu postgres drop rationale.

// prctl per-process flags the kernel tracks and reports back on the matching GET (lsys-prctl-*):
// no-new-privs is sticky (once set it can never clear), dumpable defaults to 1, pdeathsig defaults to 0.
// (g_nnp lives in container/state.c so the /proc/self/status builder can report NoNewPrivs consistently.)
static int g_dumpable = 1; // PR_SET/GET_DUMPABLE
static int g_pdeathsig;    // PR_SET/GET_PDEATHSIG
static int g_thp_disable;  // PR_SET/GET_THP_DISABLE (per-process transparent-hugepage opt-out)
static int g_subreaper;    // PR_SET/GET_CHILD_SUBREAPER (this process is a reaper for orphans)
// The process EFFECTIVE capability set. The container starts as full root (all caps); we don't model
// per-capability ENFORCEMENT in general, but we DO track what capset(2) leaves in the effective set so the
// few prctl options the kernel gates on a specific capability (PR_SET_SECUREBITS / PR_CAPBSET_DROP need
// CAP_SETPCAP) return -EPERM after that cap has been dropped -- exactly as LTP prctl02 (which drops
// CAP_SETPCAP via libcap before those subtests) expects. g_cap_eff/g_cap_bnd are DEFINED in
// container/state.c (default = the 14-cap docker set DD_CAP_DEFAULT, which INCLUDES CAP_SETPCAP) so the
// /proc/self/status builder and these handlers share one source of truth. capset() narrows the effective set.
#define CAP_SETPCAP 8
// personality(2) persona (lsys-personality): query with 0xffffffff, set returns the previous value.
static unsigned g_persona;

// ===================== sched_setscheduler / sched_*param family =====================
// macOS has no Linux scheduling policies, so dd does not actually change host scheduling; it validates the
// arguments exactly as the Linux kernel does (policy/priority/pid/pointer -> EINVAL/ESRCH/EFAULT) and keeps
// a per-process record of the requested policy+priority so sched_getscheduler/sched_getparam round-trip.
static int g_sched_policy = 0; // SCHED_OTHER
static int g_sched_prio;       // sched_priority last set
#define DD_SCHED_RESET_ON_FORK 0x40000000

// Priority band for a policy (Linux sched_get_priority_min/max): FIFO(1)/RR(2) use 1..99, every other
// valid policy uses 0..0. Returns 0 for a known policy (filling *lo/*hi), -1 for an unknown one (EINVAL).
static int sched_prio_band(int policy, int *lo, int *hi) {
    switch (policy) {
    case 1:
    case 2:
        *lo = 1;
        *hi = 99;
        return 0; // SCHED_FIFO / SCHED_RR
    case 0:
    case 3:
    case 5:
        *lo = 0;
        *hi = 0;
        return 0; // SCHED_OTHER / SCHED_BATCH / SCHED_IDLE
    default: return -1;
    }
}

// Does guest pid `gpid` name a live task? pid 0 (and the container init pid) is the caller itself; init pid 1
// maps to its real host pid. A guest THREAD tid (glibc's pthread_getaffinity_np passes pd->tid, which the
// JVM/Go/etc. do heavily) is a dd-internal id (g_next_tid, base 1000) that is NOT a host pid -- probing it
// with kill() checks an unrelated host process and wrongly returns ESRCH (the JVM's pthread_getattr_np then
// fails "pthread_getattr_np failed with error = 3"). Resolve those against the live-thread registry FIRST;
// only a pid that is neither the caller, init, nor a known guest thread falls through to the host kill()
// probe (another guest PROCESS pid passes through 1:1). Returns 0 if it exists, -ESRCH otherwise. Caller
// rejects gpid<0 (EINVAL) before calling.
static int sched_pid_live(int gpid) {
    if (gpid == 0 || gpid == container_pid()) return 0;
    if (gpid == 1 && g_init_hostpid) return 0; // guest init
    if (thread_tid_alive(gpid)) return 0;      // a live guest thread of THIS process (pd->tid)
    // guest-pid namespace: in container mode a pid this container did not create is NOT visible here -> ESRCH.
    // The old raw kill((pid_t)gpid, 0) probe leaked the existence of (and let sched_* operate on) ARBITRARY
    // same-user host processes outside the container -- the same host-pid authority leak the kill/pidfd paths
    // close via container_host_member. A genuine in-container peer is a registry member -> still resolvable.
    if (g_init_hostpid) return container_host_member(gpid) ? 0 : -ESRCH;
    if (kill((pid_t)gpid, 0) == 0) return 0; // bare (non-container) mode: historical host-pid probe
    return (errno == ESRCH) ? -ESRCH : 0;    // EPERM etc. -> the task exists, just not signalable
}

// Write a host `struct rusage` into the guest's 144-byte Linux `struct rusage` layout: the field offsets
// differ and ru_maxrss is kilobytes on Linux vs bytes on macOS. Zeroes the buffer first. Shared by
// getrusage/wait4/waitid so those all report Linux-scale accounting instead of raw Darwin byte values.
static void rusage_to_linux(uint8_t *d, const struct rusage *ru) {
    memset(d, 0, 144);
    *(int64_t *)(d + 0) = ru->ru_utime.tv_sec;
    *(int64_t *)(d + 8) = ru->ru_utime.tv_usec;
    *(int64_t *)(d + 16) = ru->ru_stime.tv_sec;
    *(int64_t *)(d + 24) = ru->ru_stime.tv_usec;
    *(int64_t *)(d + 32) = ru->ru_maxrss / 1024; // macOS bytes -> Linux KB
    *(int64_t *)(d + 64) = ru->ru_minflt;
    *(int64_t *)(d + 72) = ru->ru_majflt;
    *(int64_t *)(d + 88) = ru->ru_inblock;
    *(int64_t *)(d + 96) = ru->ru_oublock;
    *(int64_t *)(d + 120) = ru->ru_nsignals;
    *(int64_t *)(d + 128) = ru->ru_nvcsw;
    *(int64_t *)(d + 136) = ru->ru_nivcsw;
}

static int svc_proc(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                    uint64_t a5) {
    switch (nr) {
    // personality(persona): 0xffffffff queries (returns the current persona); any other value sets it and
    // returns the PREVIOUS persona. We track the persona word (incl. ADDR_NO_RANDOMIZE) so it round-trips.
    case 92: {
        unsigned prev = g_persona;
        if ((unsigned)a0 != 0xffffffffu) g_persona = (unsigned)a0;
        G_RET(c) = (uint64_t)prev;
        break;
    }
    // ===================== Process & scheduling — clone/exec/wait/ids/prctl/futex/caps/sched =====================
    // capget(hdrp, datap): the container runs as root, so report every capability present -- but ALSO
    // honour the kernel's ABI-version negotiation, which libcap-ng/libcap (and thus setpriv) probe
    // for. hdrp->version selects the layout; an UNSUPPORTED value makes the real kernel rewrite it to its
    // preferred version (v3) and fail EINVAL. The old stub ignored the header entirely and always returned
    // 0, so libcap-ng negotiated a bogus (0) version and capng_apply() then failed WITHOUT setting errno
    // -> setpriv aborts "activate capabilities: Success" before it ever reaches capset. Model it properly.
    case 90: {
        // header (version + pid = 8 bytes) must be readable; NULL / outside the address space -> EFAULT
        // (LTP capget02 "bad address header"). guest_bad_ptr also catches a PROT_NONE tst_get_bad_addr page.
        if (!a0 || guest_bad_ptr(a0, 8)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint32_t ver = *(uint32_t *)a0;
        int u32s; // number of __user_cap_data_struct the version spans
        switch (ver) {
        case 0x19980330: u32s = 1; break; // _LINUX_CAPABILITY_VERSION_1 (1 u32 mask)
        case 0x20071026:                  // _LINUX_CAPABILITY_VERSION_2 (deprecated)
        case 0x20080522: u32s = 2; break; // _LINUX_CAPABILITY_VERSION_3 (2 u32 masks, 64 caps)
        default:
            // kernel cap_validate_magic: rewrite header->version to its preferred (v3). A pure version
            // probe (data==NULL) then succeeds; otherwise it is EINVAL (LTP capget02 "bad version" +
            // the libcap-ng negotiation probe). The rewrite is what the test asserts on afterwards.
            *(uint32_t *)a0 = 0x20080522;
            G_RET(c) = a1 ? (uint64_t)(-EINVAL) : 0;
            goto cap_done;
        }
        // header->pid selects the target task: <0 -> EINVAL, a dead pid -> ESRCH (LTP capget02
        // "bad pid"/"unused pid"). 0/self/our own tid/pid resolve to this process (capget01 uses getpid()).
        int tpid = *(int *)(a0 + 4);
        if (tpid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (tpid != 0 && tpid != container_pid() && tpid != (int)getpid() && !thread_tid_alive(tpid) &&
            kill((pid_t)tpid, 0) < 0 && errno == ESRCH) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        }
        // datap (a1) is {effective, permitted, inheritable}[u32s]; NULL on a pure version probe. A bad
        // non-NULL datap -> EFAULT (kernel copy_to_user; capget02 "bad address data"). Report the guest's
        // ACTUAL effective set -- g_cap_eff, narrowed by any capset() drop (e.g. a dropped CAP_NET_RAW,
        // LTP capget01 / task D) -- rather than a blanket all-ones that over-reports capabilities.
        if (a1) {
            if (guest_bad_ptr(a1, (size_t)u32s * 12)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            uint32_t *d = (uint32_t *)a1;
            for (int i = 0; i < u32s; i++) {
                uint32_t eff = (i == 0) ? (uint32_t)g_cap_eff : (uint32_t)(g_cap_eff >> 32);
                // permitted = the docker default 14-cap set (DD_CAP_DEFAULT), NOT a blanket all-ones: a
                // default `docker run` root container has CapPrm=00000000a80425fb, matching /proc/self/status
                // exactly. The old 0xffffffff over-reported caps (e.g. CAP_SYS_ADMIN) the container lacks.
                uint32_t prm = (i == 0) ? (uint32_t)DD_CAP_DEFAULT : (uint32_t)(DD_CAP_DEFAULT >> 32);
                d[i * 3 + 0] = eff; // effective: the guest's live effective set (respects drops)
                d[i * 3 + 1] = prm; // permitted: the docker default bounding/permitted set
                d[i * 3 + 2] = 0;   // inheritable: empty (Docker default)
            }
        }
        G_RET(c) = 0;
    cap_done:
        break;
    }
    // capset(hdrp, datap): reject an unsupported ABI version the same way the kernel does (EINVAL, header
    // rewritten to v3), so a libcap-ng probe sees a consistent kernel; otherwise honour the request (the
    // container is root -- we don't model per-cap enforcement, so any well-formed set "succeeds").
    case 91: {
        if (a0 && host_range_mapped(a0, 4)) {
            uint32_t ver = *(uint32_t *)a0;
            if (ver != 0x19980330 && ver != 0x20071026 && ver != 0x20080522) {
                *(uint32_t *)a0 = 0x20080522;
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
        }
        // a capset re-raises EFFECTIVE caps from the PERMITTED set (the only bits it may set). After a
        // KEEPCAPS uid drop this is how setpriv restores effective CAP_SETGID so its following setresgid works.
        cred_init();
        g_cap_setid_eff = g_cap_setid_perm;
        // Track the effective set the guest just asked for, so a capability-gated prctl (PR_SET_SECUREBITS /
        // PR_CAPBSET_DROP) reflects a dropped CAP_SETPCAP. datap is {effective,permitted,inheritable}[u32s];
        // effective words are at data[i*3+0]. v1 spans the low 32 caps, v3 the full 64.
        if (a1 && host_range_mapped(a1, 12)) {
            uint32_t ver = (a0 && host_range_mapped(a0, 4)) ? *(uint32_t *)a0 : 0x20080522u;
            int u32s = (ver == 0x19980330u) ? 1 : 2;
            uint32_t *d = (uint32_t *)a1;
            if (u32s == 1 || host_range_mapped(a1, 24)) {
                uint64_t eff = d[0];
                if (u32s == 2) eff |= (uint64_t)d[3] << 32;
                g_cap_eff = eff;
            }
        }
        G_RET(c) = 0;
        break;
    }
    // chroot(path): re-root the guest WITHIN the rootfs jail. Resolve the target through the active jail to
    // its host backing -- this validates it exists as a directory inside the rootfs and can NEVER name a
    // host path -- then record it as the new chroot prefix. Subsequent absolute guest paths are walked
    // under this prefix yet stay confined to g_root_fd, so the guest cannot escape to the real host fs.
    case 51: {
        char gabs[4200];
        abs_guest(-100, (const char *)nonpie_p(a0), gabs, sizeof gabs); // (AT_FDCWD, path) -> guest-view abs
        char hp[4200];
        const char *h = xresolve_overlay(gabs, hp, sizeof hp); // host backing (honors any chroot already set)
        struct stat st;
        if (stat(h, &st) < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        if (!S_ISDIR(st.st_mode)) {
            G_RET(c) = (uint64_t)(-ENOTDIR);
            break;
        }
        char nc[4200];
        chroot_apply(gabs, nc, sizeof nc);                          // fold under any active chroot -> rootfs-abs
        snprintf(g_chroot, sizeof g_chroot, "%s", nc[1] ? nc : ""); // chroot("/") clears (rootfs IS the root)
        rc_reset(); // drop cached guest->host path mappings -- they predate the re-root
        G_RET(c) = 0;
        break;
    }
    case 93:
        c->exited = 1;
        c->exit_code = (int)a0;
        // exit: end THIS thread
        break;
    // exit_group: end the whole process
    case 94:
        if (getenv("PROF"))
            fprintf(stderr,
                    "[prof] crossings=%llu syscalls=%llu ibtc_miss=%llu branch_cross=%llu translations=%llu lse=%llu "
                    "wx_toggles=%llu dualmap=%d xlate_ms=%.3f mtibtc=%d mtfill=%llu futexq=%d "
                    "fwake_fast=%llu fwake_slow=%llu fwait=%llu\n",
                    (unsigned long long)g_prof_cross, (unsigned long long)g_prof_sys, (unsigned long long)g_prof_miss,
                    (unsigned long long)(g_prof_cross - g_prof_sys - g_prof_miss), (unsigned long long)g_prof_xlate,
                    (unsigned long long)g_lse_n, (unsigned long long)g_wx_toggles, g_dualmap, g_xlate_ns / 1e6,
                    g_mtibtc, (unsigned long long)g_mtfill, g_futexq, (unsigned long long)g_futex_wake_fast,
                    (unsigned long long)g_futex_wake_slow, (unsigned long long)g_futex_wait_n);
        // A3: §B shadow-return coverage. hit-rate = shret_hit / (shret_hit + shret_fb). bl_shadow /
        // bl_leaf show how the depth-gate split call sites at translate time. PROF-only (keep dark).
        if (getenv("PROF")) {
            unsigned long long h = (unsigned long long)g_prof_shret_hit, f = (unsigned long long)g_prof_shret_fb;
            double hr = (h + f) ? 100.0 * (double)h / (double)(h + f) : 0.0;
            fprintf(
                stderr,
                "[prof] shadow_push=%llu shret_hit=%llu shret_fb=%llu hit_rate=%.1f%% bl_shadow=%llu bl_leaf=%llu\n",
                (unsigned long long)g_prof_shpush, h, f, hr, (unsigned long long)g_prof_bl_shadow,
                (unsigned long long)g_prof_bl_leaf);
        }
#ifdef R_REPSTR // W4-C: x86-only rep cmps/scas idiom firing counts
        if (getenv("PROF"))
            fprintf(stderr, "[prof] repstr=%llu repstr_elems=%llu repmovs=%llu repstos=%llu\n",
                    (unsigned long long)g_repstr_n, (unsigned long long)g_repstr_elems, (unsigned long long)g_repmovs_n,
                    (unsigned long long)g_repstos_n);
#endif
#ifdef G_PROF_EXTRA
        G_PROF_EXTRA; // W5B: x86 tier-2 promotion counters
#endif
        ep_prof_dump(); // w3e: flush epoll kevent-syscall counter (atexit is bypassed by _exit)
        md_dump();      // MAPDUMP: translation-map + code-cache dump for offline PC attribution (profiling)
        if (g_noexit) { // W3D fork-server prewarm: don't kill the resident parent; unwind run_guest instead
            c->exited = 1;
            c->exit_code = (int)a0;
            break;
        }
#ifdef PCACHE_SAVE_HOOK
        PCACHE_SAVE_HOOK; // opt8: persist the translated arena before the one-shot _exit (DDJIT_PCACHE only)
#endif
        futex_robust_exit(c); // robust mutexes still held by the calling thread -> OWNER_DIED + wake waiters
        acct_proc_leave();    // release this process's cgroup accounting slot (_exit bypasses atexit)
        proc_reg_unlink();    // drop our /proc process-table entry (_exit bypasses the atexit handler)
        poslk_on_exit();      // release this process's in-engine fcntl advisory locks
        sysv_on_exit();       // apply SEM_UNDO + GC this container's SysV objects (_exit skips atexit)
        _exit((int)a0);
    case 96:
        // set_tid_address(tidptr): store tidptr as this thread's clear_child_tid so thread exit zeroes it and
        // FUTEX_WAKEs a joiner (futex_wake_addr on c->ctid). Returns the caller's TID (gettid, not the tgid).
        c->ctid = a0;
        G_RET(c) = (uint64_t)cpu_tid(c);
        break;
    case 97: {
        // unshare(flags): no real namespaces here, but honour Linux's flag validation so a probe of an
        // unknown flag (e.g. 0xdeadbeef) fails EINVAL instead of a fake success that misleads isolation setup.
        unsigned uf = (unsigned)a0;
        const unsigned UNSHARE_OK = 0x80u /*NEWTIME*/ | 0x200u /*FS*/ | 0x400u /*FILES*/ | 0x20000u /*NEWNS*/ |
                                    0x40000u /*SYSVSEM*/ | 0x2000000u /*NEWCGROUP*/ | 0x4000000u /*NEWUTS*/ |
                                    0x8000000u /*NEWIPC*/ | 0x10000000u /*NEWUSER*/ | 0x20000000u /*NEWPID*/ |
                                    0x40000000u /*NEWNET*/;
        G_RET(c) = (uf & ~UNSHARE_OK) ? (uint64_t)(int64_t)(-EINVAL) : 0;
        break;
    }
    // setns(fd, nstype): no real namespaces, but a negative/invalid fd must fail EBADF (Linux copies the ns fd
    // first). Fake success on setns(-1, ...) would let isolation setup proceed on a false premise.
    case 268:
        G_RET(c) = ((int)a0 < 0) ? (uint64_t)(int64_t)(-EBADF) : 0;
        break;
    // futex
    case 98: // futex(uaddr, op, val, timeout|nr_wake2=a3, uaddr2=a4, val3=a5); a3 is a timespec* for WAIT
        // ops and a wake count for WAKE_OP -- pass it both ways, the op selects the interpretation.
        G_RET(c) = (uint64_t)futex_op(c, (int *)a0, (int)a1 & 0x7f, (int)a2, (struct timespec *)a3, (int)a3, (int *)a4,
                                      (uint32_t)a5);
        break;
    // set_robust_list(head, len): record the per-thread robust-list head (walked on exit to mark OWNER_DIED +
    // wake robust-mutex waiters). Linux rejects len != sizeof(struct robust_list_head) (24 on LP64).
    case 99:
        if ((size_t)a1 != 24) {
            G_RET(c) = (uint64_t)(-EINVAL);
        } else {
            c->robust_list = a0;
            G_RET(c) = 0;
        }
        break;
    // syslog
    case 116: G_RET(c) = 0; break;
    // sched_setaffinity(pid, size, MASK=a2) -- record the requested mask (intersected with the online
    // set) so a later getaffinity reflects the pin; -EINVAL if it selects no online CPU, as on Linux.
    case 122: {
        size_t n = (size_t)a1;
        if (n > sizeof g_affinity) n = sizeof g_affinity;
        if (a2 && n) {
            uint8_t online[sizeof g_affinity];
            cpu_online_mask(online, sizeof online);
            uint8_t want[sizeof g_affinity];
            int any = 0;
            for (size_t i = 0; i < n; i++) {
                want[i] = ((const uint8_t *)a2)[i] & online[i];
                if (want[i]) any = 1;
            }
            if (!any) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            memset(g_affinity, 0, sizeof g_affinity);
            memcpy(g_affinity, want, n);
            g_affinity_set = 1;
        }
        G_RET(c) = 0;
        break;
    }
    case 123: {
        size_t n = (size_t)a1;
        // sched_getaffinity(pid,size,MASK=a2!) -- return the current mask (all online CPUs by default),
        // not just CPU 0, so CPU_COUNT() and tcmalloc's enumeration see the real width (mongod aborts).
        // Linux validates the cpusetsize FIRST: it must be a multiple of sizeof(long) AND wide enough to
        // hold every online CPU, else -EINVAL (LTP sched_getaffinity01). The old handler skipped this and
        // always "succeeded", so a deliberately-tiny cpusetsize wrongly returned 0.
        if ((n & (sizeof(unsigned long) - 1)) || n * 8 < (size_t)dd_online_cpus()) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // The target task must exist. Linux looks the pid up AFTER the size check and BEFORE the copy-out,
        // returning -ESRCH for a pid that names no live task (LTP sched_getaffinity01 uses an unused pid).
        // pid 0 == the caller; a live guest thread tid (glibc's pthread_getaffinity_np -> pd->tid, on the
        // JVM/Go bootstrap path) resolves via the registry, not a host kill() of an unrelated host pid.
        if (sched_pid_live((int)(int32_t)a0) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        // The mask itself must be writable -> EFAULT on a bad pointer (matches Linux copy_to_user).
        if (a2 && n && !host_range_mapped((uintptr_t)a2, n < 128 ? n : 128)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (n > 128) n = 128;
        if (a2 && n) memcpy((void *)a2, affinity_mask(), n);
        // Return the number of bytes the mask spans (glibc zeroes the remainder); 8 covers <=64 CPUs.
        G_RET(c) = n < 8 ? (uint64_t)n : 8;
        break;
    }
    // sched_yield
    case 124: G_RET(c) = 0; break;
    // ---- sched_setscheduler / sched_*param arg-validation family (LTP sched_*01..03). dd has no real
    // Linux scheduling classes, so these validate exactly like the kernel and record the requested
    // policy/priority for round-trip reads; the errno ORDER matches the kernel line-for-line.
    // sched_setparam(pid, param)
    case 118: {
        int pid = (int)a0;
        if (!a1 || pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // do_sched_setscheduler: !param||pid<0
        if (guest_bad_ptr(a1, sizeof(int))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        int prio;
        memcpy(&prio, (void *)a1, sizeof(int));
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        }
        int lo, hi;
        sched_prio_band(g_sched_policy, &lo, &hi); // current policy is always valid
        if (prio < lo || prio > hi) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        g_sched_prio = prio;
        G_RET(c) = 0;
        break;
    }
    // sched_setscheduler(pid, policy, param)
    case 119: {
        int pid = (int)a0, policy = (int)a1;
        if (!a2 || pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // !param || pid<0
        if (guest_bad_ptr(a2, sizeof(int))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        } // copy_from_user(param)
        int prio;
        memcpy(&prio, (void *)a2, sizeof(int));
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        } // find_process_by_pid
        int base = policy & ~DD_SCHED_RESET_ON_FORK, lo, hi;
        if (sched_prio_band(base, &lo, &hi) < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // unknown policy
        if (prio < lo || prio > hi) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // priority out of band
        // Real-time classes (SCHED_FIFO=1 / SCHED_RR=2) need CAP_SYS_NICE or a nonzero RLIMIT_RTPRIO.
        // The container runs unprivileged, so the kernel rejects them with EPERM after arg validation --
        // otherwise a latency-sensitive probe believes RT scheduling was installed when nothing changed.
        if (base == 1 || base == 2) {
            G_RET(c) = (uint64_t)(-EPERM);
            break;
        }
        g_sched_policy = base;
        g_sched_prio = prio;
        G_RET(c) = 0;
        break;
    }
    // sched_getscheduler(pid)
    case 120: {
        int pid = (int)a0;
        if (pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        }
        G_RET(c) = (uint64_t)g_sched_policy;
        break;
    }
    // sched_getparam(pid, param)
    case 121: {
        int pid = (int)a0;
        if (!a1 || pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // kernel: !param || pid<0
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        }
        if (guest_bad_ptr(a1, sizeof(int))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        memcpy((void *)a1, &g_sched_prio, sizeof(int)); // struct sched_param{int sched_priority}
        G_RET(c) = 0;
        break;
    }
    // sched_get_priority_max(policy)
    case 125: {
        int lo, hi;
        if (sched_prio_band((int)a0, &lo, &hi) < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        G_RET(c) = (uint64_t)hi;
        break;
    }
    // sched_get_priority_min(policy)
    case 126: {
        int lo, hi;
        if (sched_prio_band((int)a0, &lo, &hi) < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        G_RET(c) = (uint64_t)lo;
        break;
    }
    // sched_rr_get_interval(pid, tp): report a nominal RR quantum (100ms). Validation order matches the
    // kernel: pid<0 -> EINVAL, missing task -> ESRCH, bad tp -> EFAULT.
    case 127: {
        int pid = (int)a0;
        if (pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        }
        if (guest_bad_ptr(a1, sizeof(struct timespec))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        ((uint64_t *)a1)[0] = 0;         // tv_sec
        ((uint64_t *)a1)[1] = 100000000; // tv_nsec = 100ms
        G_RET(c) = 0;
        break;
    }
    case 140: {
        // setpriority(which, who, prio). Linux CLAMPS the resulting nice to [-20, 19]; macOS PRIO_MAX is
        // 20, so an unclamped host setpriority(...,>=20) leaves nice==20 and a following getpriority reads
        // 20 -- the nice02 off-by-one ("Process priority 20, expected 19"). Clamp to the Linux range first.
        // `which` is validated as on Linux (EINVAL); the priority set itself stays best-effort success (the
        // container is root, so a host EACCES/EPERM for lowering nice must not surface to a root guest).
        int which = (int)a0;
        if (which != PRIO_PROCESS && which != PRIO_PGRP && which != PRIO_USER) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int prio = (int)a2;
        if (prio > 19)
            prio = 19;
        else if (prio < -20)
            prio = -20;
        setpriority(which, (int)a1, prio);
        G_RET(c) = 0;
        break;
    }
    case 141: {
        // getpriority(which, who) -> Linux raw kernel encoding (20 - nice). Linux validates `which` first
        // (EINVAL for anything but PRIO_PROCESS/PGRP/USER), then fails ESRCH when no process matches
        // (which,who) -- e.g. getpriority02's who==-1. macOS can report the wrong errno family here, so
        // enforce the Linux contract directly: bad which -> EINVAL, any other failure -> ESRCH.
        int which = (int)a0;
        if (which != PRIO_PROCESS && which != PRIO_PGRP && which != PRIO_USER) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        errno = 0;
        int r = getpriority(which, (int)a1);
        if (r == -1 && errno) { // a real -1 nice value keeps errno==0; only a genuine failure sets it
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        G_RET(c) = (uint64_t)(20 - r);
        break;
    }
    // setuid(uid): a privileged task sets real+eff+saved; an unprivileged one may only set euid to an id
    // it already holds. Honoured against the credential overlay so apt's _apt drop (and its "can't regain
    // root" check) behave as on Linux. (See cred_init above.)
    case 146: {
        cred_init();
        int u = (int)a0;
        if (!uid_permitted(u)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (g_cap_setid_eff) g_ruid = g_suid = u;
        g_euid = u;
        g_fsuid_ovr = -1;   // fsuid follows the new euid (POSIX) -> new files stamped with it
        cred_uid_changed(); // recompute CAP_SETID after the uid transition (drop vs keepcaps)
        G_RET(c) = 0;
        break;
    }
    // setgid(gid): symmetric to setuid above.
    case 144: {
        cred_init();
        int gg = (int)a0;
        if (!gid_permitted(gg)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (g_cap_setid_eff) g_rgid = g_sgid = gg;
        g_egid = gg;
        g_fsgid_ovr = -1; // fsgid follows the new egid
        G_RET(c) = 0;
        break;
    }
    // setresuid(ruid,euid,suid): each (uid_t)-1 leaves that id unchanged; every requested id must be
    // permitted (privileged, or already held). glibc's seteuid() arrives here as setresuid(-1,euid,-1).
    case 147: {
        cred_init();
        int r = (int)a0, e = (int)a1, s = (int)a2;
        if (!uid_permitted(r) || !uid_permitted(e) || !uid_permitted(s)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (r != -1) g_ruid = r;
        if (e != -1) g_euid = e;
        if (s != -1) g_suid = s;
        g_fsuid_ovr = -1;   // fsuid follows euid
        cred_uid_changed(); // recompute CAP_SETID after the uid transition (drop vs keepcaps)
        G_RET(c) = 0;
        break;
    }
    // setresgid(rgid,egid,sgid): symmetric. glibc's setegid() arrives here as setresgid(-1,egid,-1).
    case 149: {
        cred_init();
        int r = (int)a0, e = (int)a1, s = (int)a2;
        if (!gid_permitted(r) || !gid_permitted(e) || !gid_permitted(s)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (r != -1) g_rgid = r;
        if (e != -1) g_egid = e;
        if (s != -1) g_sgid = s;
        g_fsgid_ovr = -1; // fsgid follows egid
        G_RET(c) = 0;
        break;
    }
    // setreuid(ruid,euid): -1 leaves an id unchanged. The kernel moves saved-uid to the new euid whenever
    // the real uid is changed, or the euid is set to a value other than the previous real uid.
    case 145: {
        cred_init();
        int r = (int)a0, e = (int)a1, old_ruid = g_ruid;
        if (!uid_permitted(r) || !uid_permitted(e)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (r != -1) g_ruid = r;
        if (e != -1) g_euid = e;
        if (r != -1 || (e != -1 && e != old_ruid)) g_suid = g_euid;
        g_fsuid_ovr = -1;   // fsuid follows euid
        cred_uid_changed(); // recompute CAP_SETID after the uid transition (drop vs keepcaps)
        G_RET(c) = 0;
        break;
    }
    // setregid(rgid,egid): symmetric to setreuid. -1 leaves an id unchanged; saved-gid moves to the new
    // egid when the real gid is changed, or the egid is set to a value other than the previous real gid.
    case 143: {
        cred_init();
        int r = (int)a0, e = (int)a1, old_rgid = g_rgid;
        if (!gid_permitted(r) || !gid_permitted(e)) {
            G_RET(c) = (uint64_t)(-(int64_t)EPERM);
            break;
        }
        if (r != -1) g_rgid = r;
        if (e != -1) g_egid = e;
        if (r != -1 || (e != -1 && e != old_rgid)) g_sgid = g_egid;
        g_fsgid_ovr = -1; // fsgid follows egid
        G_RET(c) = 0;
        break;
    }
    // setfsuid(fsuid) / setfsgid(fsgid): set only the FS id used for ownership checks (and, here, the id
    // that STAMPS newly-created files). Linux always returns the PREVIOUS fs id and never sets errno; the
    // change is honoured only for a permitted id. == euid/egid clears the override so it tracks the creds.
    case 151: {
        cred_init();
        int prev = newfile_uid(), u = (int)a0;
        if (u != -1 && uid_permitted(u)) g_fsuid_ovr = (u == g_euid) ? -1 : u;
        G_RET(c) = (uint64_t)(uint32_t)prev;
        break;
    }
    case 152: {
        cred_init();
        int prev = newfile_gid(), g = (int)a0;
        if (g != -1 && gid_permitted(g)) g_fsgid_ovr = (g == g_egid) ? -1 : g;
        G_RET(c) = (uint64_t)(uint32_t)prev;
        break;
    }
    case 148: {
        // getresuid(r,e,s) -- report the overlay so a runtime drop is observed (apt verifies all three).
        // Linux faults the whole call if any output pointer is NULL/unwritable (EFAULT), writing none.
        cred_init();
        if (!a0 || !a1 || !a2 || guest_bad_ptr(a0, 4) || guest_bad_ptr(a1, 4) || guest_bad_ptr(a2, 4)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        *(uint32_t *)a0 = (uint32_t)g_ruid;
        *(uint32_t *)a1 = (uint32_t)g_euid;
        *(uint32_t *)a2 = (uint32_t)g_suid;
        G_RET(c) = 0;
        break;
    }
    case 150: {
        // getresgid(r,e,s) -- report the overlay (see getresuid above). NULL/unwritable pointer -> EFAULT.
        cred_init();
        if (!a0 || !a1 || !a2 || guest_bad_ptr(a0, 4) || guest_bad_ptr(a1, 4) || guest_bad_ptr(a2, 4)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        *(uint32_t *)a0 = (uint32_t)g_rgid;
        *(uint32_t *)a1 = (uint32_t)g_egid;
        *(uint32_t *)a2 = (uint32_t)g_sgid;
        G_RET(c) = 0;
        break;
    }
    // setpgid -- bash job control. The container init has getpid()==1 (container_pid), so bash issues
    // setpgid(0, 1); forwarded verbatim that names launchd (host pid 1) -> EPERM ("initialize_job_control:
    // setpgid: Operation not permitted"). Map the faked PID1 self-reference to the host's own process, and
    // treat a residual EPERM as success -- a container is its own session, so guest process groups are virtual.
    case 154: {
        // Map the guest's view of the init (pid/pgid 1) to its real host pid/group, then do the REAL setpgid.
        // Children already carry real host pids, so they pass straight through and get real process groups.
        // EPERM is benign (the init is a session leader, already its own group leader) -> report success.
        // Linux validates the requested pgid >= 0 first (setpgid02 case 1: pgid < 0 -> EINVAL).
        if ((int)a1 < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        pid_t pid = ((pid_t)a0 == 1 && g_init_hostpid) ? g_init_hostpid : (pid_t)a0;
        pid_t pgid = ((pid_t)a1 == 1 && g_init_hostpid) ? g_init_hostpid : (pid_t)a1;
        int r = setpgid(pid, pgid);
        if (r == 0) {
            G_RET(c) = 0;
            break;
        }
        // EPERM is benign ONLY for bash's job-control self-move into the container init's own (virtual)
        // group -- setpgid(0, 1): the init is a session leader already its own group leader, so the host
        // rejects it but the container is its own session and guest groups are virtual. Gate the swallow on
        // the guest having named group 1; a genuine EPERM (setpgid02 case 3: joining a NONEXISTENT group)
        // must propagate, along with EINVAL/ESRCH (bad pgid / target that is neither caller nor its child).
        if (errno == EPERM && (pid_t)a1 == 1) {
            G_RET(c) = 0;
            break;
        }
        G_RET(c) = (uint64_t)(int64_t)(-errno);
        break;
    }
    // getpgid / getsid -- translate the init's real host group/session id to the guest's pgid 1 so the guest's
    // identity is self-consistent (getpid 1 == getpgrp 1 == getsid 1). bash then sees itself as session+group
    // leader and initializes job control WITHOUT the setpgid EPERM / "cannot set terminal process group"
    // warning -- it enables job control cleanly, and the real terminal handoff works (see TIOCSPGRP above +
    // the rt_sigprocmask stop-signal mirroring).
    case 155: {
        // Map the guest's view of the init (pid 1) to its real host pid, then query. Linux getpgid fails
        // ONLY with ESRCH (no process with that pid) -- never EPERM/EINVAL. The old handler returned the
        // raw -1 on failure, which svc_done then misread as -EPERM (errno "1"): getpgid02's -99/unused_pid
        // wrongly reported EPERM instead of ESRCH. Force ESRCH for any lookup failure.
        pid_t pid = ((pid_t)a0 == 1 && g_init_hostpid) ? g_init_hostpid : (pid_t)a0;
        pid_t r = getpgid(pid);
        if (r < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (g_init_hostpid && r == g_init_hostpid) r = 1;
        G_RET(c) = (uint64_t)r;
        break;
    }
    case 156: {
        // getsid: same contract as getpgid above -- fails only with ESRCH for a pid that names no process
        // (getsid02's unused_pid), so map a raw -1 to ESRCH rather than let svc_done coin it into EPERM.
        pid_t pid = ((pid_t)a0 == 1 && g_init_hostpid) ? g_init_hostpid : (pid_t)a0;
        pid_t r = getsid(pid);
        if (r < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (g_init_hostpid && r == g_init_hostpid) r = 1;
        G_RET(c) = (uint64_t)r;
        break;
    }
    case 158: {
        // getgroups(size, list): in rootfs mode report the IMAGE-DERIVED supplementary set runc computes
        // (parsed at container_init -- alpine root -> 0 0 1 2 3 4 6 10 11 20 26 27; ubuntu -> 0), which a
        // guest setgroups(2) may later replace (apt/gosu drop). size==0 queries the count; size<count is
        // -EINVAL; a bad list pointer is -EFAULT. This matches getgroups(2) exactly and stays byte-consistent
        // with the /proc/self/status Groups: line (both read g_groups). Bare mode (unparsed) keeps the prior
        // behavior below: the container egid when a USER-ns gid is set, else the real host set.
        if (g_groups_parsed) {
            int cnt = g_ngroups;
            if ((int)a0 == 0) {
                G_RET(c) = (uint64_t)cnt;
                break;
            } // size 0 -> just the count
            if ((int)a0 < cnt) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (a1) {
                if (!host_range_mapped((uintptr_t)a1, (size_t)cnt * sizeof(gid_t))) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                gid_t *out = (gid_t *)a1;
                for (int i = 0; i < cnt; i++)
                    out[i] = g_groups[i];
            }
            G_RET(c) = (uint64_t)cnt;
            break;
        }
        if (g_gid >= 0) {
            // getgroups -> [effective gid]. Tracking the overlay's egid means apt's drop to _apt's group
            // is reflected here too (it setgroups(1,&_apt_gid) right before switching).
            if ((int)a0 >= 1 && a1) *(gid_t *)a1 = (gid_t)cred_egid();
            G_RET(c) = 1;
            break;
        }
        int r = getgroups((int)a0, (gid_t *)a1);
        G_RET(c) = r < 0 ? (uint64_t)(-errno) : (uint64_t)r;
        break;
    }
    // setgroups(size, list): a privileged guest replaces its supplementary set (apt setgroups(1,&_apt_gid)
    // before dropping to _apt; gosu clears groups before switching user). In rootfs mode record it so
    // getgroups(2) + /proc/self/status Groups: reflect the guest's current view; size 0 clears the set. Bare
    // mode (unparsed) keeps the historical no-op-succeed. size out of range -> -EINVAL; bad list -> -EFAULT.
    case 159: {
        if (!g_groups_parsed) {
            G_RET(c) = 0;
            break;
        }
        long ng = (long)a0;
        if (ng < 0 || ng > DD_NGROUPS_MAX) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (ng > 0 && a1) {
            if (!host_range_mapped((uintptr_t)a1, (size_t)ng * sizeof(gid_t))) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            const gid_t *in = (const gid_t *)a1;
            for (long i = 0; i < ng; i++)
                g_groups[i] = in[i];
        }
        g_ngroups = (int)ng;
        G_RET(c) = 0;
        break;
    }
    // getrusage(who, *usage) -- a1 is the buffer, not a0!
    case 165: {
        struct rusage ru;
        // Linux validates `who` FIRST: only RUSAGE_SELF(0), RUSAGE_CHILDREN(-1) and RUSAGE_THREAD(1) are
        // legal; anything else is -EINVAL BEFORE the buffer is touched (LTP getrusage02 passes who=-2). The
        // old handler mapped every non-(-1) value to SELF and always "succeeded".
        int who_g = (int)a0;
        if (who_g != 0 && who_g != -1 && who_g != 1) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // RUSAGE_THREAD(1) -> SELF (macOS has no per-thread rusage; SELF is the closest faithful account).
        int who = (who_g == -1) ? RUSAGE_CHILDREN : RUSAGE_SELF;
        if (a1) {
            // The 144-byte struct rusage is written directly by the engine (not via a host syscall), so a
            // bad/unmapped pointer must return -EFAULT here rather than fault the engine (access_ok).
            if (!host_range_mapped((uintptr_t)a1, 144)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            uint8_t *d = (uint8_t *)a1;
            // Linux struct rusage layout (18 longs)
            memset(d, 0, 144);
            if (getrusage(who, &ru) == 0) rusage_to_linux(d, &ru);
        }
        G_RET(c) = 0;
        break;
    }
    // prctl(option,...)
    case 167: {
        if ((int)a0 == 15) {
            // PR_SET_NAME: kernel copies up to 16 bytes from arg2 -> -EFAULT on an unreadable pointer
            // (LTP prctl02 PR_SET_NAME/bad_addr). Validate before the deref.
            if (guest_bad_ptr((uintptr_t)a1, 1)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            snprintf(g_procname, sizeof g_procname, "%.15s", (const char *)a1);
            G_RET(c) = 0;
            break;
        } // PR_SET_NAME
        if ((int)a0 == 16) {
            if (guest_bad_ptr((uintptr_t)a1, 16)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            snprintf((char *)a1, 16, "%s", g_procname);
            G_RET(c) = 0;
            break;
        } // PR_GET_NAME
        // PR_SET_KEEPCAPS(8)/PR_GET_KEEPCAPS(7) drive the CAP_SETID retention model -- setpriv arms
        // KEEPCAPS so its post-uid-drop capset can re-raise CAP_SETGID (see cred_uid_changed/capset).
        if ((int)a0 == 8) {
            g_keepcaps = (a1 != 0);
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 7) {
            G_RET(c) = (uint64_t)g_keepcaps;
            break;
        }
        // PR_SET_PDEATHSIG(1)/PR_GET_PDEATHSIG(2): the parent-death signal round-trips (no real delivery
        // under the JIT, but the value the guest set is reported back). arg2 must be 0 (clear) or a valid
        // signal number 1..64; anything else is -EINVAL (LTP prctl02 PR_SET_PDEATHSIG/ULONG_MAX).
        if ((int)a0 == 1) {
            if (a1 > 64) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            g_pdeathsig = (int)a1;
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 2) {
            if (guest_bad_ptr((uintptr_t)a1, sizeof(int))) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            *(int *)a1 = g_pdeathsig;
            G_RET(c) = 0;
            break;
        }
        // PR_GET_DUMPABLE(3)/PR_SET_DUMPABLE(4): the dumpable flag round-trips. SET accepts ONLY
        // SUID_DUMP_DISABLE(0) and SUID_DUMP_USER(1); any other value (incl. 2 = the internal
        // SUID_DUMP_ROOT, which is not settable from userspace) is -EINVAL (LTP prctl02 PR_SET_DUMPABLE/2).
        if ((int)a0 == 3) {
            G_RET(c) = (uint64_t)(unsigned)g_dumpable;
            break;
        }
        if ((int)a0 == 4) {
            if (a1 > 1) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            g_dumpable = (int)a1;
            G_RET(c) = 0;
            break;
        }
        // PR_SET_NO_NEW_PRIVS(38)/PR_GET_NO_NEW_PRIVS(39): sticky once set; SET requires arg2==1.
        if ((int)a0 == 38) {
            if (a1 != 1 || a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            g_nnp = 1;
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 39) {
            if (a1 || a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            G_RET(c) = (uint64_t)(unsigned)g_nnp;
            break;
        }
        // PR_SET_CHILD_SUBREAPER(36)/PR_GET_CHILD_SUBREAPER(37): the subreaper flag round-trips. SET stores
        // arg2 as a boolean; GET writes it through the int* in arg2 (LTP prctl03). NOTE: only the flag itself
        // round-trips here -- the ACTUAL reparenting of an orphaned descendant onto a subreaper is a
        // process-tree feature dd's 1:1 host-fork model does not implement (an orphaned guest grandchild is
        // reparented by the host kernel, not routed back to the guest subreaper), so prctl03's reparent/
        // SIGCHLD/wait subtests are a known process-model gap, out of this syscall layer's scope.
        if ((int)a0 == 36) {
            g_subreaper = (a1 != 0);
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 37) {
            if (guest_bad_ptr((uintptr_t)a1, sizeof(int))) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            *(int *)a1 = g_subreaper;
            G_RET(c) = 0;
            break;
        }
        // PR_GET_THP_DISABLE(42)/PR_SET_THP_DISABLE(41): the per-process transparent-hugepage opt-out flag
        // round-trips. GET rejects any nonzero unused arg; SET treats arg2 as a boolean and rejects nonzero
        // arg3/arg4/arg5 (LTP prctl02 PR_{GET,SET}_THP_DISABLE). Modeling it (rather than the old blanket
        // EINVAL) makes the feature probe succeed so its dependent LTP subtests run, matching real Linux.
        if ((int)a0 == 42) {
            if (a1 || a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            G_RET(c) = (uint64_t)(unsigned)g_thp_disable;
            break;
        }
        if ((int)a0 == 41) {
            if (a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            g_thp_disable = (a1 != 0);
            G_RET(c) = 0;
            break;
        }
        // PR_CAP_AMBIENT(47): the ambient capability set is empty in this all-root container, so RAISE/LOWER
        // are accepted no-ops and IS_SET always reports "not set"; the value of this handler is matching
        // Linux's argument validation exactly (LTP prctl02 PR_CAP_AMBIENT/*). Sub-command in arg2:
        //   4=CLEAR_ALL (arg3/4/5 must be 0), 2=RAISE, 3=LOWER, 1=IS_SET (arg3=cap, must be <= CAP_LAST_CAP;
        //   arg4/5 must be 0). Any other sub-command is -EINVAL.
        if ((int)a0 == 47) {
            if (a1 == 4) { // PR_CAP_AMBIENT_CLEAR_ALL
                if (a2 || a3 || a4) {
                    G_RET(c) = (uint64_t)(-EINVAL);
                    break;
                }
                G_RET(c) = 0;
                break;
            }
            if (a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            } // arg4/arg5 unused for the rest
            if (a1 == 1 || a1 == 2 || a1 == 3) { // IS_SET / RAISE / LOWER
                if (a2 > 40 /* CAP_LAST_CAP (CAP_CHECKPOINT_RESTORE) */) {
                    G_RET(c) = (uint64_t)(-EINVAL);
                    break;
                }
                G_RET(c) = (a1 == 1) ? 0 /* IS_SET: not in the (empty) ambient set */ : 0;
                break;
            }
            G_RET(c) = (uint64_t)(-EINVAL); // unknown sub-command
            break;
        }
        // PR_GET_SPECULATION_CTRL(52): report a plausible speculation-control status. arg3/arg4/arg5 must be
        // 0 (LTP prctl02 PR_GET_SPECULATION_CTRL/arg-nonzero -> EINVAL); the feature must NOT report EINVAL
        // for the all-zero probe, or its dependent subtests would be skipped where real Linux runs them.
        if ((int)a0 == 52) {
            if (a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            G_RET(c) = 2; // PR_SPEC_PRCTL is off, mitigation not forced: PR_SPEC_ENABLE
            break;
        }
        // PR_CAPBSET_READ(23): "is capability arg2 in this task's BOUNDING set?" capsh --print / getpcaps
        // probe every cap this way to render the mask; it MUST agree with /proc/self/status CapBnd. Returns 1
        // if present, 0 if absent, -EINVAL for a cap index past CAP_LAST_CAP (40). The docker default holds
        // exactly the 14 bits of DD_CAP_DEFAULT (g_cap_bnd), so e.g. CAP_SYS_ADMIN(21) reads 0.
        if ((int)a0 == 23) {
            if (a1 > 40 /* CAP_LAST_CAP */) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            G_RET(c) = (uint64_t)((g_cap_bnd >> a1) & 1ull);
            break;
        }
        // PR_GET_SECCOMP(21): the docker default seccomp profile is always applied, so real docker reports
        // filter mode (2) here AND as Seccomp:2 in /proc/self/status. Match it (unfiltered Linux returns 0);
        // software that gates behaviour on being sandboxed reads this. arg2..5 are ignored by the kernel.
        if ((int)a0 == 21) {
            G_RET(c) = 2;
            break;
        }
        // PR_SET_SECCOMP(22): the legacy entry point for seccomp, ENFORCED like the seccomp(2) syscall
        // (rare.c case 277) via os/linux/seccomp.c. arg2 is the SECCOMP_MODE_* (STRICT=1, FILTER=2 -- note
        // these differ from seccomp(2)'s op numbers); FILTER takes the struct sock_fprog* in arg3 (a2).
        if ((int)a0 == 22) {
            if (a1 == 1 /*SECCOMP_MODE_STRICT*/)
                G_RET(c) = (uint64_t)(int64_t)seccomp_set_strict();
            else if (a1 == 2 /*SECCOMP_MODE_FILTER*/)
                G_RET(c) = (uint64_t)(int64_t)seccomp_install_filter(a2, 0);
            else
                G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // PR_SET_PTRACER (0x59616d61, "Yama"): Chromium/crashpad allows a specific helper pid to ptrace it
        // when Linux's Yama LSM is present. dd has no Yama policy to enforce, so accept the request as a no-op.
        if ((int)a0 == 1499557217) {
            G_RET(c) = 0;
            break;
        }
        // PR_SET_SECUREBITS(28) and PR_CAPBSET_DROP(24) require CAP_SETPCAP in the effective set; without it
        // the kernel returns -EPERM before any further validation (LTP prctl02 drops CAP_SETPCAP first). With
        // the cap held (the container default) they succeed for a well-formed argument.
        if ((int)a0 == 28) {
            if (!(g_cap_eff & (1ull << CAP_SETPCAP))) {
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            }
            G_RET(c) = 0; // securebits accepted (we don't enforce them, but the value round-trips as 0)
            break;
        }
        if ((int)a0 == 24) {
            if (!(g_cap_eff & (1ull << CAP_SETPCAP))) {
                G_RET(c) = (uint64_t)(-EPERM);
                break;
            }
            if (a1 > 40 /* CAP_LAST_CAP */) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            g_cap_bnd &= ~(1ull << a1); // actually drop the bit so PR_CAPBSET_READ/CapBnd stay consistent
            G_RET(c) = 0;
            break;
        }
        // 0 for known no-ops; EINVAL for unknown (kernel does)
        switch ((int)a0) {
        case 15:
        case 35:
        case 53:
        case 55:
        // NAME/SECCOMP/TIMERSLACK/THP/SPECCTRL...
        case 59: G_RET(c) = 0; break;
        // EINVAL -- so feature probes (e.g. magic "AUXV") fail as on Linux
        default:
            G_RET(c) = (uint64_t)(-22);
            break;
        }
        break;
    }
    // getpid (PID ns: init -> 1)
    case 172: G_RET(c) = (uint64_t)container_pid(); break;
    case 173:
        // getppid (init's parent is 0 in the ns). A restored process reports its recorded guest parent
        // (g_self_gppid), since its live host parent differs after the tree was re-forked.
        if (g_self_gppid >= 0)
            G_RET(c) = (uint64_t)g_self_gppid;
        else
            G_RET(c) = (container_pid() == 1) ? 0 : (uint64_t)getppid();
        break;
    // getuid/geteuid -> container uid (0=root by default), reflecting any runtime drop (apt -> _apt).
    case 174:
        cred_init();
        G_RET(c) = (uint64_t)g_ruid;
        break;
    case 175: G_RET(c) = (uint64_t)cred_euid(); break;
    // getgid/getegid
    case 176:
        cred_init();
        G_RET(c) = (uint64_t)g_rgid;
        break;
    case 177: G_RET(c) = (uint64_t)cred_egid(); break;
    // gettid -- a UNIQUE per-thread id (unlike getpid, which is the shared tgid). The init thread keeps
    // c->tid==0 and reports the container pid (==1, where tid==tgid as on Linux); each spawned thread
    // carries its own id (spawn_thread). A correct gettid is load-bearing for runtimes that key thread
    // state on it (e.g. Go stores it in m.procid and tgkill()s it to preempt) -- collapsing every thread
    // to tid 1 makes their cross-thread signalling target the wrong thread and live-lock.
    case 178: G_RET(c) = (uint64_t)(c->tid ? c->tid : container_pid()); break;
    // clone(flags,stack,ptid,tls,ctid)
    case 220: {
        // CLONE_THREAD: stack arg IS the top
        if (a0 & 0x10000) {
            G_RET(c) = (uint64_t)spawn_thread(c, a0, a1, a3, a2, a4);
            break;
        }
        // cgroup pids.max also gates a FORKED PROCESS: a forked child is a new container task, so a fork
        // past the limit must fail EAGAIN exactly as clone(CLONE_THREAD) does (the container-wide count is
        // one shared budget across the process tree). Previously only in-process threads were gated.
        if (g_pids_max && acct_pids_total() >= g_pids_max) {
            G_RET(c) = (uint64_t)(int64_t)(-EAGAIN);
            break;
        }
        // fork/vfork: COW copy; child continues. Flush RAM-backed scratch into the real (shared) fds so
        // parent and child see one coherent file via the inherited description, exactly as POSIX requires
        // (the heap-resident buffers would otherwise COW-diverge while the fd stays shared).
        memf_materialize_all();
        sigexit_init(); // create the shared guest-signal-death relay in the PARENT before forking, so
                        // this child (and its descendants) inherit the same MAP_SHARED page it may die into.
        uint64_t fk0 = forkprof_on() ? now_ns() : 0; // parent-side fork latency (DD_FORKPROF=1)
        pid_t pid = fork();
        if (pid == 0) {
            // clone(CLONE_VM, child_stack): glibc posix_spawn/popen/vfork pass a separate child stack in a1
            // and seed the clone trampoline (fn ptr + args) at its top. We fork() (COW) instead of sharing
            // the VM, but the child MUST run on a1 or glibc reads the trampoline off the parent's SP ->
            // garbage branch (SIGILL — broke initdb). a1==0 for a plain fork (bash), keeping the inherited SP.
            if ((a0 & 0x100) && a1) G_SP(c) = a1;
            fork_child_hooks(c); // shared child-side engine reset (cache re-alias, caches, kqueues, locks)
            if (w7_on()) w7_trace("clone-child flags=0x%llx (fresh host pid)", (unsigned long long)a0);
            // CLONE_CHILD_SETTID(0x01000000): store the child's own tid (== its pid for a process clone) into
            // the child's *ctid (a4). CLONE_CHILD_CLEARTID(0x00200000): remember ctid so thread/process exit
            // zeroes it and FUTEX_WAKEs a joiner. The old code ignored both, so pthread/runtime handshakes
            // that read the child tid from these slots saw stale memory.
            if ((a0 & 0x01000000) && a4) *(int *)a4 = (int)getpid();
            if (a0 & 0x00200000) c->ctid = a4;
        } else if (fk0) {
            fprintf(stderr, "[forkprof] parent fork()=%llu ns\n", (unsigned long long)(now_ns() - fk0));
        }
        // CLONE_PIDFD(0x1000): the kernel stores a pidfd for the new child at the address in `parent_tid`
        // (a2, the aarch64 clone slot). macOS has no pidfd, so mint a kqueue that fires on the child's exit
        // (pidfd_make); modern runtimes (Go/Rust/glibc posix_spawn) then epoll_wait/poll THAT fd to reap the
        // compiler child they just forked. Without it the guest's pidfd storage keeps its stale value (Go
        // seeds it 0 -> fd 0 = stdin) and the wait blocks forever at 0% CPU -- the go/npm/cargo build hang.
        if (pid > 0 && (a0 & 0x1000) && a2) {
            int pfd = pidfd_make(pid);
            if (pfd >= 0) *(int *)a2 = pfd;
        }
        if (pid > 0) { // parent side of a successful fork: count it for /proc/stat processes + pids.current
            atomic_fetch_add(&g_forks_since_boot, 1);
            proc_reg_mark_child((int)pid); // guest-pid namespace: register the child NOW (parent-side, race-
                                           // free) so a kill/pidfd membership check can never ESRCH it before
                                           // it runs its own proc_reg_after_fork publish
            acct_child_born((int)pid); // register the child's OWN task slot (container-wide pids.current)
        }
        // CLONE_PARENT_SETTID(0x00100000): store the child's tid (its pid) into the PARENT's *ptid (a2).
        // Mutually exclusive with CLONE_PIDFD (which also uses the ptid slot), so it never clobbers a pidfd.
        if (pid > 0 && (a0 & 0x00100000) && !(a0 & 0x1000) && a2) *(int *)a2 = (int)pid;
        // parent: pid, child: 0
        G_RET(c) = pid < 0 ? (uint64_t)(-errno) : (uint64_t)pid;
        // A fork/vfork that was normalized to clone repurposed the guest's arg registers; put them back so
        // the syscall preserves every GPR but rax, as the real kernel does (no-op for a genuine clone).
        G_FORK_PRESERVE(c);
        break;
    }
    // execveat(dirfd, path, argv, envp, flags) -- canonical 281 (x86 322 maps here via sysmap). Resolve
    // (dirfd, path, flags) to a guest-absolute exec path, shift the args into execve positions, and fall
    // through to the shared case-221 body (glibc fexecve() and Rust/Go re-exec helpers use this).
    case 281: {
        static char eat_pb[4200]; // static: referenced via a0 across the fallthrough into case 221
        const char *ep = (const char *)a1;
        int eflags = (int)a4;
        if (ep && !ep[0] && (eflags & 0x1000)) { // AT_EMPTY_PATH: exec the file dirfd itself names (fexecve)
            char hb[4200];
            if ((int)a0 < 0 || fcntl((int)a0, F_GETPATH, hb) != 0 || !hb[0]) {
                G_RET(c) = (uint64_t)(int64_t)(((int)a0 < 0) ? -EBADF : -EACCES);
                break;
            }
            if (g_rootfs) {
                char gb[4200];
                guest_from_host_raw(hb, gb, sizeof gb);
                snprintf(eat_pb, sizeof eat_pb, "%s", gb);
            } else
                snprintf(eat_pb, sizeof eat_pb, "%s", hb);
        } else if (!ep || !ep[0]) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOENT); // empty path without AT_EMPTY_PATH
            break;
        } else {
            if (eflags & 0x100) { // AT_SYMLINK_NOFOLLOW: a symlink final component is ELOOP, never followed
                char lpb[4200];
                struct stat ls;
                const char *lp = atpath((int)a0, ep, lpb, sizeof lpb, 1);
                if (fstatat(ATFD(a0), lp, &ls, AT_SYMLINK_NOFOLLOW) == 0 && S_ISLNK(ls.st_mode)) {
                    G_RET(c) = (uint64_t)(int64_t)(-ELOOP);
                    break;
                }
            }
            if (ep[0] == '/')
                snprintf(eat_pb, sizeof eat_pb, "%s", ep);
            else if (g_rootfs)
                abs_guest((int)a0, ep, eat_pb, sizeof eat_pb);
            else if ((int)a0 == -100)
                snprintf(eat_pb, sizeof eat_pb, "%s", ep); // bare cwd-relative: case 221 resolves it as-is
            else { // bare mode, relative to a real dirfd: absolutize via the fd's host path
                char hb[4200];
                if (fcntl((int)a0, F_GETPATH, hb) != 0) {
                    G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                    break;
                }
                snprintf(eat_pb, sizeof eat_pb, "%s/%s", hb, ep);
            }
        }
        a0 = (uint64_t)(uintptr_t)eat_pb;
        a1 = a2; // argv
        a2 = a3; // envp
    }
        /* fall through */
    // execve(path, argv, envp)
    case 221: {
        memf_materialize_all(); // non-CLOEXEC scratch fds survive exec -> flush RAM into the real files
        // Linux comm = last component of the path PASSED to execve, captured BEFORE the /proc magic-link
        // rewrite below and before binfmt_script (execve("/proc/self/exe") -> comm "exe"; "./run.sh"
        // keeps "run.sh"). Applied at the committed point further down, only once the exec cannot fail.
        char comm_src[256];
        {
            const char *cb = (const char *)a0, *cs = cb ? strrchr(cb, '/') : NULL;
            snprintf(comm_src, sizeof comm_src, "%s", cs ? cs + 1 : (cb ? cb : ""));
        }
        // exec THROUGH the /proc magic links: execve("/proc/self/exe") (busybox re-exec, daemons,
        // test harnesses) and execve("/proc/self/fd/N") (glibc fexecve fallback) must exec the link's
        // TARGET -- the rootfs /proc is empty, so resolving them as ordinary paths ENOENTed.
        static char pse_pb[4200];
        {
            char lnk[4200];
            const char *xp = (const char *)a0;
            if (proc_self_exe(xp, lnk, sizeof lnk)) {
                snprintf(pse_pb, sizeof pse_pb, "%s", lnk);
                a0 = (uint64_t)(uintptr_t)pse_pb;
            } else {
                int pfn = procfd_num(xp);
                if (pfn >= 0) {
                    char hb[4200];
                    if (fcntl(pfn, F_GETPATH, hb) == 0 && hb[0]) {
                        if (g_rootfs) {
                            char gb[4200];
                            guest_from_host_raw(hb, gb, sizeof gb);
                            snprintf(pse_pb, sizeof pse_pb, "%s", gb);
                        } else
                            snprintf(pse_pb, sizeof pse_pb, "%s", hb);
                        a0 = (uint64_t)(uintptr_t)pse_pb;
                    }
                }
            }
        }
        char pb[4200];
        const char *p =
            // resolve the exec path through the SAME resolver openat uses (atpath): overlay-aware
            // (upper then lowers), bind-mount/volume aware, AND relative-path aware -- a RELATIVE exec
            // (`./x`, `./binary` from `go build`/`make`, `./script`) is joined to the guest cwd (g_cwd),
            // not the host cwd. The old xresolve_overlay bailed on any non-'/' path and returned it raw,
            // so `./x` was access()'d against the host process cwd (never the mounted guest cwd) -> ENOENT.
            atpath(-100, (const char *)a0, pb, sizeof pb, 0);
        if (access(p, F_OK) != 0) {
            G_RET(c) = (uint64_t)(-2);
            break;
            // ENOENT
        }
        char *argv[DD_MAXARGV]; // Linux allows far more than 255 args within ARG_MAX -- a fixed 256 silently
        int ac = 0;             // dropped the tail (a different command ran, and /proc/self/cmdline diverged)
        uint64_t *gv = (uint64_t *)a1; // a1 (argv array base) already nonpie_p()'d at the top redirect
        while (gv && gv[ac] && ac < DD_MAXARGV - 1) {
            argv[ac] = (char *)nonpie_p(gv[ac]); // each argv[] element may itself be a low-image pointer
            ac++;
        }
        argv[ac] = NULL;
        if (w7_on()) {
            const char *ty = "none";
            for (int i = 0; i < ac; i++)
                if (argv[i] && !strncmp(argv[i], "--type=", 7)) ty = argv[i] + 7;
            w7_trace("execve type=%s argc=%d argv0=%s", ty, ac, (ac > 0 && argv[0]) ? argv[0] : "");
        }
        // Forward the guest's ACTUAL environment across the exec: build_stack rebuilds the new process env
        // from DD_GUEST_ENV, so serialize envp (a2) into it NOW while guest memory is still mapped. A guest
        // that set/modified env vars (FOO=bar, a tweaked PATH) thus sees them survive; a NULL envp keeps the
        // container's DD_GUEST_ENV defaults (a2 is NOT rebased by the dispatch redirect, unlike a0/a1).
        exec_forward_env(a2);
        // Capture the guest-absolute exec path NOW (a0 is still mapped) so /proc/self/exe can name the new
        // image after the teardown below. ld.so resolves a binary's $ORIGIN (DT_RUNPATH) via readlink of
        // /proc/self/exe; a stale value makes an exec'd dynamic binary fail to find its own libraries (e.g.
        // rustup's proxy execs the real rustc, whose RUNPATH $ORIGIN/../lib must point into the toolchain).
        char gexe[4200];
        if (g_rootfs)
            abs_guest(-100, (const char *)a0, gexe, sizeof gexe);
        else
            // bare mode: abs_guest would join the untracked g_cwd ("/"); keep the raw path and let
            // exe_canon below join the LIVE host cwd (the engine chdir()s for real without a rootfs)
            snprintf(gexe, sizeof gexe, "%s", (const char *)a0);
        // shebang: exec the #! interpreter instead (resolve_shebang_chain is shared with the initial loader).
        // RECURSIVE -- the interpreter may itself be a #! script (e.g. /usr/bin/env -> coreutils multicall);
        // resolve the whole chain (Linux binfmt_script, up to SHEBANG_MAX levels) and load the FINAL interp.
        char sh_store[SHEBANG_MAX * 2][256], shpb[4200];
        char *na[DD_MAXARGV];
        int nn = 0;
        // Linux passes the execve path (a0) as the script-path arg; the original argv[0] is discarded.
        na[nn++] = (char *)a0;
        for (int i = 1; i < ac && nn < DD_MAXARGV - 1; i++)
            na[nn++] = argv[i];
        na[nn] = NULL;
        const char *sh_finalhost;
        int sh_new = resolve_shebang_chain(na, nn, DD_MAXARGV, p, sh_store, shpb, sizeof shpb, &sh_finalhost);
        if (sh_new < 0) {
            // too many nested #! -> ELOOP. `-ELOOP` is the HOST (macOS, 62) errno; svc_done's m2l_errno
            // maps it to Linux ELOOP (40) at the syscall boundary, exactly like the vfs symlink-loop path.
            G_RET(c) = (uint64_t)(-ELOOP);
            break;
        }
        if (sh_new != nn) { // a shebang chain resolved -> load the final interpreter, not the script
            snprintf(gexe, sizeof gexe, "%s", na[0]); // /proc/self/exe names the interpreter
            // the final interp host is already overlay-resolved (the #! interp, e.g. /bin/sh, may live only
            // in a read-only lower in a fresh container; the chain resolves each level through the overlay)
            p = sh_finalhost;
            if (access(p, F_OK) != 0) {
                G_RET(c) = (uint64_t)(-2);
                break;
            }
            for (int i = 0; i <= sh_new; i++)
                argv[i] = na[i];
            ac = sh_new;
        }
        // /proc/self/exe must name the new image as an ABSOLUTE, CANONICAL guest path -- fold "."/".."
        // and resolve symlinks to the backing file (an exec of /bin/sh -> busybox reports /bin/busybox,
        // and a relative "./x" exec reports "<cwd>/x", exactly like Linux d_path). glibc static-pie
        // asserts on a non-canonical value at startup (dl-origin.c).
        {
            char gcanon[4200];
            exe_canon(gexe, gcanon, sizeof gcanon);
            snprintf(gexe, sizeof gexe, "%s", gcanon);
        }
        // Committed to the exec now (all ENOENT early-returns are behind us). execve makes the process
        // single-threaded -- the kernel terminates every OTHER thread in the group -- so before we flush the
        // address space and CLOEXEC fds below, tear down any sibling guest threads (a Go all-threads setuid,
        // e.g. gosu/su-exec, leaves netpoller/idle Ms live; a surviving M would run the old image against the
        // freed state). Blocks until all peers have left run_guest, so the teardown below is race-free.
        thread_exit_others(c);
        set_guest_comm(comm_src); // comm := basename of the exec'd NAME (captured pre-rewrite above)
        cred_after_exec();        // exec recomputes caps (non-root loses them) + clears KEEPCAPS; ids persist
#ifdef PCACHE_SAVE_HOOK
        // the exec below flushes this image's translated arena and RE-KEYS the cache identity for
        // the new image (pcache_exec_reload), so the exit-time save can never again cover this epoch.
        // Persist the outgoing image under its OWN (current) key now -- e.g. the `sh` of a `sh -c tar`
        // chain, which otherwise never gets cached because the shell always ends in an exec. Every save
        // refusal gate applies unchanged (fork child, restored-from-cache, poisoned, SMC, mixed-base); a
        // restored epoch records its revival stats instead (pcache_warm_note, the policy input).
        // Single-threaded here by construction (thread_exit_others above), so the snapshot cannot tear.
        PCACHE_SAVE_HOOK;
#endif
        // emulate the kernel's close-on-exec sweep. No real host exec runs below -- we re-load the new image
        // in this same process -- so FD_CLOEXEC fds must be closed by hand or they leak into the new program.
        exec_close_cloexec();
        sysv_after_exec(); // detach SysV shm + clear semadj across execve (registry itself survives)
        // Tear down the inherited guest address space before loading the new image: a post-fork exec
        // otherwise keeps the parent's DENSE layout, and load_elf must bias a non-PIE ET_EXEC off its
        // fixed vaddr (__PAGEZERO blocks the low 4 GB) -> its baked absolute refs collide -> SIGSEGV.
        // argv + path live in guest memory we're about to munmap, so copy them to the host heap first.
        char *xpath = strdup(p);
        char *xargv[DD_MAXARGV];
        for (int i = 0; i < ac && i < DD_MAXARGV - 1; i++)
            xargv[i] = strdup(argv[i]);
        xargv[ac < DD_MAXARGV - 1 ? ac : DD_MAXARGV - 1] = NULL;
        gmap_reset_all();
        gna_reset();                   // the old image's PROT_NONE ranges are gone with its address space
        mlk_reset();                   // ... and so are its mlock'd ranges (VmLck resets across execve)
        g_nonpie_lo = g_nonpie_hi = 0; // reset; load_elf re-sets it iff the new main image is non-PIE
#ifdef R_REPSTR // g_nonpie_blob_code lives in the x86-only frontend (translate/x86_64/translate.c); guard so this
                // shared execve path still compiles into the aarch64 unity (R_REPSTR is defined only by cpu_x86_64.h)
        g_nonpie_blob_code = 0; // reset; load_elf re-sets it iff the new main image carries the V8 blob
#endif
        g_go_iscgo = 0; // reset; load_elf re-sets it iff the new main image is a cgo Go image
        p = xpath;
        for (int i = 0; i < ac && i < DD_MAXARGV - 1; i++)
            argv[i] = xargv[i];
        argv[ac < DD_MAXARGV - 1 ? ac : DD_MAXARGV - 1] = NULL;
        struct loaded lm;
        char pc_ihost[4200];
        const char *pc_interp_host = NULL;
        (void)pc_ihost;
        (void)pc_interp_host;
#ifdef PCACHE_EXEC_HOOKS
        pcache_exec_force_main(); // map the new image at the fixed VA so its cached arena is reusable
#endif
        load_elf(p, &lm);
        uint64_t jump = lm.entry, at_base = 0;
        char interp[256];
        if (elf_interp(p, interp, sizeof interp) == 0) {
            char ib[4200];
            // follow+confine ld.so symlink (through the overlay)
            const char *ih = xresolve_overlay(interp, ib, sizeof ib);
#ifdef PCACHE_EXEC_HOOKS
            snprintf(pc_ihost, sizeof pc_ihost, "%s", ih); // outlive `ib` for the cache id below
            pc_interp_host = pc_ihost;
            pcache_exec_force_interp();
#endif
            struct loaded li;
            load_elf(ih, &li);
            jump = li.entry;
            at_base = li.base;
        }
        g_cp = g_cache;
        memset(g_map, 0, sizeof g_map);
        // flush old translations
        g_npend = 0;
        memset(g_ibtc, 0, sizeof g_ibtc);
#ifdef PCACHE_EXEC_HOOKS
        // the new image is loaded + the arena is flushed -> try to restore its warm translated arena
        // from the persistent cache (this is what makes the go-build fork+execve storm fast). Graceful MISS
        // translates fresh + saves on exit.
        pcache_exec_reload(p, pc_interp_host, argv[0], jump);
#endif
        // execve is a wholesale code-cache flush (g_cp reset + g_map/g_ibtc zeroed above), so it must ALSO
        // run the per-arch wholesale-flush hook the dispatcher uses (jit/dispatch.c) -- not just the lighter
        // fork/exec G_SHADOW_RESET. On x86 that hook drops the 2-way g_xibtc (G_SHADOW_RESET is a NO-OP there,
        // so g_xibtc was surviving execve); on aarch64 it resets the §B shadow stack. Without it a forked
        // child that execve's a new image (apt http method / gzip / cc1 / git child) keeps the OLD image's
        // g_xibtc entries -- keyed by guest PC the new image REUSES, bodies pointing into the freed cache --
        // and an indirect branch resolves into garbage host code -> SIGSEGV/SIGBUS (/ /).
        G_SHADOW_CLEAR(c);
        // POSIX execve resets CAUGHT signal handlers to SIG_DFL (SIG_IGN stays ignored). Without this, a
        // handler the calling shell installed (e.g. busybox sh's SIGCHLD job-control handler) survives into
        // the new image and is later delivered to a now-garbage handler address -> crash (redis/valkey run
        // via `sh -c …`). handler>1 == a real caught handler; 0=DFL, 1=IGN.
        for (int s = 1; s < 65; s++)
            if (g_sigact[s].handler > 1) {
                g_sigact[s].handler = 0;
                g_sigact[s].flags = 0;
                g_sigact[s].mask = 0;
            }
        uint8_t *heap = mmap(NULL, 256u << 20, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANON, -1, 0);
        gmap_add((uint64_t)heap, 256u << 20);
        brk_lo = brk_cur = (uint64_t)heap;
        brk_hi = brk_lo + (256u << 20);
        uint64_t sp = build_stack(ac, argv, &lm, at_base);
        proc_reg_publish(gexe, ac, argv); // republish the process table entry (comm/argv changed on exec)
        free(xpath);
        for (int i = 0; i < ac && i < 255; i++)
            free(xargv[i]);
        snprintf(g_exe_path_store, sizeof g_exe_path_store, "%s", gexe); // /proc/self/exe -> the new image
        g_exe_path = g_exe_path_store;
        G_RESET_REGS(c);
        c->nzcv = 0;
        G_TLS(c) = 0;
        G_SP(c) = sp;
        G_PC(c) = jump;
        // jump to new program; don't advance pc
        c->redirect = 1;
        break;
    }
    // wait4(pid, *status, opts, *rusage)
    case 260: {
        int st = 0;
        pid_t r;
        // Linux validates the option bits BEFORE any child lookup: anything outside
        // WNOHANG|WUNTRACED|WCONTINUED|__WNOTHREAD|__WALL|__WCLONE is -EINVAL (waitpid04 case 3 passes
        // options 0xffffffff and expects EINVAL, not the ECHILD a permissive host wait4 returns). macOS
        // ignores unknown bits, so gate here rather than trust the host.
        if ((int)a2 & ~(int)0xE000000B) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // waitpid(INT_MIN, ...) is the one negative pid Linux answers with ESRCH rather than ECHILD:
        // pid < -1 means "any child in process group -pid", and -INT_MIN overflows, so the kernel
        // special-cases it to -ESRCH (waitpid04 case 4). Any other invalid pgroup is a normal ECHILD.
        if ((int)a0 == INT_MIN) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        // checkpoint restore: a wait targeting a specific checkpoint-time guest pid must name the live host
        // pid the tree was re-forked with (identity no-op on a normal launch, g_pidmap_n==0).
        if (g_pidmap_n && (int)a0 > 0)
            a0 = (uint64_t)(unsigned)pidmap_to_live((int)a0);
        else if (g_pidmap_n && (int)a0 < -1)
            a0 = (uint64_t)(int64_t)(-pidmap_to_live(-(int)a0));
        // when ptrace is already in use in this session (a tracee link exists -> nactive>0) route the
        // wait through the ptrace pump, which surfaces tracee ptrace-stops (Linux-encoded) AND real child
        // exits and tears a link down when its tracee dies. For the ENTIRE non-ptrace matrix nactive is 0,
        // so this predicate is false. Returns 1 when it produced a result (r/st Linux-encoded).
        if (ptrace_wait_active()) {
            pid_t pr;
            int handled = ptrace_wait(c, (pid_t)(int)a0, (int)a2, (struct rusage *)a3, &st, &pr);
            if (handled) {
                if (pr < 0) {
                    G_RET(c) = (uint64_t)(int64_t)pr;
                    break;
                } // -errno / -EINTR
                if (a1) *(int *)a1 = st;
                G_RET(c) = (uint64_t)pr;
                break;
            }
        }
        // tracer-wait race guard (see ptrace.c pt_wait_arm): a child may PTRACE_TRACEME + stop AFTER
        // this parent already blocked here (the classic strace ordering: parent waitpid()s before the child
        // traces itself, so nactive was 0 at entry and we take this plain path). To let the tracee's stop
        // SIGCHLD interrupt us so we can reroute, arm a benign SIGCHLD handler ONLY around the BLOCKING
        // wait4 (a2 without WNOHANG), and ONLY if the guest has no SIGCHLD handler of its own; it is
        // restored the instant the wait returns. This touches NOTHING outside this one blocking wait4 --
        // no other syscall is affected, the guest's waitpid never returns a spurious EINTR (the do/while
        // retries), and a guest that never calls wait4 is never armed. pt_wait_arm returns 0 (no-op) for a
        // WNOHANG wait, a guest with its own SIGCHLD handler, or if the ptrace arena is absent.
        // Translate Linux wait4 options to the host's (they DIVERGE): Linux WCONTINUED is 0x8, but that value
        // is macOS WSTOPPED -- passing the raw bits made a WCONTINUED wait miss continued children and mis-
        // encode the following status. Only WNOHANG/WUNTRACED share a value; the __W* thread-selection bits
        // have no host form and are dropped. rusage goes into a LOCAL host struct and is converted to the
        // guest's Linux layout after the reap (a raw host rusage buffer would leave Darwin byte-scale values
        // in the Linux ru_maxrss/... fields).
        int mopt = 0;
        if ((int)a2 & 1) mopt |= WNOHANG;
        if ((int)a2 & 2) mopt |= WUNTRACED;
        if ((int)a2 & 8) mopt |= WCONTINUED;
        struct rusage ruloc;
        memset(&ruloc, 0, sizeof ruloc);
        if (a3 && !host_range_mapped((uintptr_t)a3, 144)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        struct sigaction pt_saved;
        int pt_armed = ((int)a2 & 1 /*WNOHANG*/) ? 0 : pt_wait_arm(&pt_saved);
        // SA_RESTART: a wait interrupted by a handler that asked to restart (e.g. a SIGCHLD reaper, or
        // gcc's driver) must transparently retry instead of failing the guest with EINTR.
        ts_wait_enter(); // 'S' while blocked waiting on a child (WNOHANG returns immediately, harmless)
        do {
            r = wait4((pid_t)(int)a0, &st, mopt, a3 ? &ruloc : NULL);
            // Reroute to the ptrace pump if the interrupt was a tracee of ours stopping (we became a tracer
            // while blocked). Gated on nactive>0 -> the non-ptrace matrix never enters this branch.
            if (r < 0 && errno == EINTR && ptrace_wait_active() && ptrace_any_tracee_of_self()) {
                pid_t pr;
                if (ptrace_wait(c, (pid_t)(int)a0, (int)a2, (struct rusage *)a3, &st, &pr)) {
                    pt_wait_disarm(pt_armed, &pt_saved);
                    ts_wait_leave();
                    if (pr < 0) {
                        G_RET(c) = (uint64_t)(int64_t)pr;
                        goto wait_done;
                    }
                    if (a1) *(int *)a1 = st;
                    G_RET(c) = (uint64_t)pr;
                    goto wait_done;
                }
            }
        } while (r < 0 && SVC_EINTR_RESTART(c));
        pt_wait_disarm(pt_armed, &pt_saved);
        ts_wait_leave();
        if (r < 0) {
            G_RET(c) = (uint64_t)(-errno);
            break;
        }
        // WIFSIGNALED: macOS termsig -> Linux, and encode WCOREDUMP (0x80) exactly as Linux does. The host
        // child dies from a real host signal (signal.c default action), but macOS almost never writes a core
        // for it (cores off by default) and the guest's setrlimit(RLIMIT_CORE) is not applied to the host, so
        // the host status usually lacks 0x80. Synthesize the bit from (core-dumping signal AND the guest's
        // RLIMIT_CORE soft limit > 0) -- the Linux rule -- while still honoring the host's own core flag if it
        // did dump. Non-core signals (SIGKILL/SIGTERM/...) or rlim_cur==0 => no bit (WCOREDUMP false).
        int rawsig = st & 0x7f;
        // WIFCONTINUED: macOS encodes a continued child as a "stopped" status whose stop-signal is SIGCONT
        // (low byte 0x7f, high byte 19); Linux uses the sentinel status 0xffff. Check this BEFORE the stopped
        // branch below, which would otherwise mistranslate it as a stop.
        if ((st & 0xff) == 0x7f && ((st >> 8) & 0xff) == SIGCONT) {
            st = 0xffff;
        } else if (rawsig != 0 && rawsig != 0x7f) {
            int lsig = sig_m2l(rawsig) & 0x7f;
            int core = sig_coredumps(lsig) && (((st & 0x80) != 0) || svc_core_rlimit_cur() > 0);
            st = (st & ~0xff) | lsig | (core ? 0x80 : 0);
        }
        // WIFSTOPPED: macOS stopsig -> Linux
        else if ((st & 0xff) == 0x7f)
            st = (st & ~0xff00) | ((sig_m2l((st >> 8) & 0xff) & 0xff) << 8);
        // WIFEXITED from the host, but the child may have relayed a guest signal death: a fatal-default
        // signal with no faithful fatal host mapping is delivered by the child _exit()ing after recording its
        // Linux signo in the shared table. Reconstruct the SIGNALED status here. A genuine guest _exit(n)
        // recorded nothing, so it is left as WIFEXITED(n).
        else if ((st & 0x7f) == 0) {
            int gsig, gcore;
            if (sigexit_lookup(r, &gsig, &gcore, 1)) st = (gsig & 0x7f) | (gcore ? 0x80 : 0);
        }
        // Fill the guest's Linux-layout rusage from the reaped child's host accounting (kilobyte-scaled).
        if (a3 && r > 0) rusage_to_linux((uint8_t *)a3, &ruloc);
        if (a1) *(int *)a1 = st;
        // guest-pid namespace: a reaped child that TERMINATED (exited or signalled -- not merely stopped
        // 0x7f / continued 0xffff) leaves the pid table; drop its container-registry record here so a
        // signal-killed child (which never ran its own exit cleanup) can't leave a stale membership marker
        // that a recycled host pid could inherit. Use the host pid `r` before the restore remap below.
        if (r > 0 && (st & 0xff) != 0x7f && st != 0xffff) proc_reg_reap((int)r);
        // checkpoint restore: report the reaped child under the guest pid the checkpoint recorded, and drop
        // its translation once it is reaped so a future host pid can never alias it (no-op on normal launch).
        if (g_pidmap_n && r > 0) {
            int gp = pidmap_to_guest((int)r);
            if (gp != (int)r) {
                if (((st & 0x7f) == 0) || (((st & 0x7f) != 0x7f) && ((st & 0x7f) != 0))) pidmap_del_live((int)r);
                r = (pid_t)gp;
            }
        }
        G_RET(c) = (uint64_t)r;
    wait_done:; // the EINTR reroute jumps here (G_RET + *status already set)
        break;
    }
    case 261: {
        // prlimit64(pid, resource, NEW, OLD): report the CURRENT limit into OLD first (so a combined
        // get+set returns the pre-change value), THEN apply NEW into the per-resource store so a later
        // get reflects it. glibc's getrlimit/setrlimit/prlimit all funnel through this syscall, so the
        // store (g_ulimit, also seeded by docker --ulimit) is the single source of truth. without
        // applying NEW, setrlimit "succeeded" but the value never took -- the next getrlimit saw the old.
        int res = (int)a1;
        // Linux validates BEFORE touching the limits: the task lookup runs first (a negative or dead target
        // pid -> ESRCH), then the resource number is range-checked (>= RLIM_NLIMITS(16) -> EINVAL). Without
        // these dd reports success for dead pids and unsupported resources, so probes see them as valid.
        if ((int)a0 < 0 || sched_pid_live((int)a0) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (res < 0 || res >= DD_RLIM_MAX) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (a3) svc_fill_rlimit(res, (uint64_t *)a3);
        if (a2) {
            const uint64_t *nl = (const uint64_t *)a2;
            uint64_t ncur = nl[0], nmax = nl[1];
            // Linux: soft may not exceed hard -> EINVAL (RLIM_INFINITY == ~0 is the max, so it never trips).
            if (ncur != ~0ull && nmax != ~0ull && ncur > nmax) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (res >= 0 && res < DD_RLIM_MAX) {
                g_ulimit[res].set = 1;
                g_ulimit[res].cur = ncur;
                g_ulimit[res].max = nmax;
            }
        }
        G_RET(c) = 0;
        break;
    }
    // clone3(clone_args*, size)
    case 435: {
        // clone3(clone_args*, size): a hostile/buggy guest can pass a bad args pointer or a junk size;
        // validate BEFORE any deref so it returns an errno instead of faulting the engine. -EINVAL if size
        // is below the VER0 clone_args (we read only its first 64 bytes) or implausibly large; -EFAULT if
        // the args struct isn't mapped.
        if (a1 < 64 || a1 > 4096) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (!host_range_mapped(a0, a1)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint64_t *ca = (uint64_t *)a0;
        uint64_t flags = ca[0];
        // CLONE_THREAD: sp = stack + stack_size
        if (flags & 0x10000) {
            G_RET(c) = (uint64_t)spawn_thread(c, flags, ca[5] + ca[6], ca[7], ca[3], ca[2]);
            break;
        }
        // cgroup pids.max gates a clone3 forked PROCESS too (see case 220): fork past the limit -> EAGAIN.
        if (g_pids_max && acct_pids_total() >= g_pids_max) {
            G_RET(c) = (uint64_t)(int64_t)(-EAGAIN);
            break;
        }
        sigexit_init(); // shared signal-death relay must exist in the parent before fork (see case 220)
        pid_t pid = fork();
        // child: the same shared engine reset as the clone/fork site above (cache re-alias / §B shadow /
        // path caches / kqueues / fork-unsafe locks). clone3 historically lacked the W^X re-assert and the
        // DIR*-cache drop the clone site had; the shared helper closes that drift.
        if (pid == 0) {
            if ((flags & 0x100) && ca[5]) G_SP(c) = ca[5] + ca[6];
            fork_child_hooks(c);
            // clone_args: child_tid = ca[2]. CLONE_CHILD_SETTID stores the child's tid there; CLONE_CHILD_
            // CLEARTID remembers it so exit zeroes it + wakes a joiner (mirrors case 220).
            if ((flags & 0x01000000) && ca[2]) *(int *)ca[2] = (int)getpid();
            if (flags & 0x00200000) c->ctid = ca[2];
        }
        // CLONE_PIDFD: clone3 stores the child pidfd via the `pidfd` field (clone_args[1]); back it the same
        // way as case 220 so a clone3-based spawn (newer glibc/runtimes) can epoll_wait/poll it to reap.
        if (pid > 0 && (flags & 0x1000) && ca[1]) {
            int pfd = pidfd_make(pid);
            if (pfd >= 0) *(int *)ca[1] = pfd;
        }
        if (pid > 0) { // parent side of a successful clone3 fork: count it (see case 220)
            atomic_fetch_add(&g_forks_since_boot, 1);
            proc_reg_mark_child((int)pid); // guest-pid namespace: parent-side registration (see case 220)
            acct_child_born((int)pid); // register the child's OWN task slot (container-wide pids.current)
        }
        // clone_args: parent_tid = ca[3]. CLONE_PARENT_SETTID stores the child's tid (pid) into the PARENT's
        // parent_tid (a distinct field from pidfd in clone3, so it never conflicts with CLONE_PIDFD).
        if (pid > 0 && (flags & 0x00100000) && ca[3]) *(int *)ca[3] = (int)pid;
        G_RET(c) = pid < 0 ? (uint64_t)(-errno) : (uint64_t)pid;
        break;
    }
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
