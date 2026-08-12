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

// e_machine this engine can translate; execve rejects any other ELF with ENOEXEC. R_REPSTR is defined only
// by the x86-64 frontend's cpu.h (the same discriminator the execve body already uses further down).
#ifdef R_REPSTR
#define HL_EXEC_ELF_MACHINE 0x3Eu // EM_X86_64
#else
#define HL_EXEC_ELF_MACHINE 0xB7u // EM_AARCH64
#endif

// execve env forwarding: serialize the guest's envp array into HL_GUEST_ENV (the "K=V\nK=V..." string
// build_stack reads when laying out the new process stack), so the guest's actual environment crosses the
// re-exec. A guest-initiated exec makes the guest's envp AUTHORITATIVE (like Linux): whatever the guest
// passes -- including an EMPTY set for envp==NULL -- is EXACTLY what the new program sees. We mark it with
// HL_GUEST_ENV_EXACT so build_stack injects NONE of the engine's fallback defaults (PATH/HOME/LANG/
// GLIBC_TUNABLES). Those defaults are only appropriate on the INITIAL container launch (the daemon never
// sets HL_GUEST_ENV_EXACT); a guest execve that curates its env -- or clears it -- must match Linux, where
// `execve(path, argv, NULL)` yields an empty environment and `execve(path,argv,["FOO=bar"])` yields exactly
// one entry. Each pointer may be a low non-PIE address, so rebase the array base and every element with
// nonpie_p(), exactly as the argv loop does. hl_option_set() copies the buffer, so it survives the teardown.
static void exec_forward_env(uint64_t envp_guest) {
    if (!envp_guest) {
        // Linux: NULL envp -> the new program runs with an EMPTY environment. Publish an empty, authoritative
        // env (do not leak stale initial HL_GUEST_ENV data) and flag it exact so build_stack adds
        // no defaults -> the guest sees envc==0, byte-exact with the native oracle.
        hl_process_guest_environment_set("");
        hl_option_set("HL_GUEST_ENV_ESC", "1", 1);
        hl_option_set("HL_GUEST_ENV_EXACT", "1", 1);
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
        // '\n' is HL_GUEST_ENV's record separator, but Linux permits newline (and any non-NUL byte) inside an
        // env value. Escape '\\'->"\\\\" and '\n'->"\\n" so a raw '\n' only ever marks a record boundary;
        // build_stack unescapes when HL_GUEST_ENV_ESC=1. Worst case each byte becomes 2 -> reserve 2*el+2.
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
        buf[len++] = '\n'; // HL_GUEST_ENV record separator (build_stack splits on '\n')
        buf[len] = 0;
    }
    hl_process_guest_environment_set(buf);
    hl_option_set("HL_GUEST_ENV_ESC", "1", 1);   // tell build_stack the records are escape-encoded
    hl_option_set("HL_GUEST_ENV_EXACT", "1", 1); // guest-initiated exec: this env is authoritative, inject no defaults
    free(buf);
}

// Fill a guest `struct rlimit { rlim_cur; rlim_max; }` for {get,set}rlimit/prlimit64 (cases 163/261).
// Shared so both forms report identical limits. Most resources are unlimited, but a few MUST be finite or
// guests size data structures off them: RLIMIT_STACK(3) reports the conventional 8MB main-stack size, and
// RLIMIT_NOFILE(7) reports a finite fd cap (soft 20480 / hard 1048576, the docker container default) -- a
// guest like memcached does calloc(rlim_cur, sizeof(conn)), which overflows if the soft limit is RLIM_INFINITY.
static void svc_fill_rlimit(int resource, uint64_t *o) {
    // Docker --ulimit override wins (g_limits, seeded from HL_ULIMITS in state.c): a guest that reads its
    // limits (memcached calloc's off RLIMIT_NOFILE, the JVM sizes threads off RLIMIT_NPROC) must see the
    // requested value, not the hl default. `set` gates each resource so unspecified ones keep the defaults.
    if (hl_limit_table_get(&g_limits, resource, &o[0], &o[1])) {
        if (resource == 7) {
            uint32_t guest_limit = hl_engine_guest_fd_limit();
            uint64_t ceiling = guest_limit > 0 ? guest_limit : 20480;
            if (o[1] > ceiling) o[1] = ceiling;
            if (o[0] > o[1]) o[0] = o[1];
        }
        return;
    }
    switch (resource) {
    case 3: // RLIMIT_STACK
        o[0] = 8ull << 20;
        o[1] = ~0ull;
        break;
    case 7: // RLIMIT_NOFILE -- docker container default (oracle: soft 20480, hard 1048576; was 1024/1048576)
        o[0] = 20480;
        {
            uint32_t guest_limit = hl_engine_guest_fd_limit();
            o[1] = guest_limit > 0 ? guest_limit : 20480;
        }
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
// Engine-private host fds (the rootfs/volume dir-fds and signal self-pipe) are skipped:
// they back the runtime itself and must survive the emulated exec; closing them would leave dangling fd
// numbers the new guest could reuse, corrupting timer/signal delivery and path confinement.
static int exec_fd_is_engine(int fd) {
    if (fd < 0) return 1;
    if (cmsg_inflight_is_retained(fd)) return 1;
    if (hl_host_process_fd_private_current(fd)) return 1;
    if (eventfd_peer_is_engine_fd(fd)) return 1;
    if (sfd_wr_is(fd)) return 1; // signalfd write ends are engine-private (read ends are ordinary guest fds)
    if (fd == g_root_fd) return 1;
    for (int i = 0; i < g_nvols; i++)
        if (fd == g_vols[i].fd) return 1;
    return 0;
}

static int exec_fd_is_bound(int fd) {
    return g_linux_box != NULL && fd >= 0 &&
           hl_linux_fd_snapshot_get(g_linux_box, (hl_linux_fd)fd, &(hl_linux_fd_snapshot){0}) == HL_STATUS_OK;
}

// Close the CLOEXEC guest fds among a bounded [0,maxfd) range (host enumeration fallback path only).
// The caller passes the REAL descriptor-table size (getdtablesize() = the current soft RLIMIT_NOFILE),
// so do NOT clamp it DOWN -- that would leave CLOEXEC fds above the cap open across exec. The ceiling
// only guards a garbage/negative getdtablesize() return from spinning an absurd loop.
static void exec_close_cloexec_scan(int maxfd) {
    if (maxfd < 0 || maxfd > (1 << 20)) maxfd = 4096;
    for (int fd = 0; fd < maxfd; fd++) {
        if (exec_fd_is_bound(fd)) continue;
        if (exec_fd_is_engine(fd)) continue;
        int fl = fcntl(fd, F_GETFD);
        if (fl >= 0 && (fl & FD_CLOEXEC)) {
            fd_reset_emul(fd); // drop hl's emulation tables for this fd so a reused number isn't misrouted
            close(fd);
        }
    }
}

static void exec_bound_closed(void *context, hl_linux_fd fd) {
    (void)context;
    ep_provider_retire_endpoint((int)fd);
    ep_object_retire_endpoint((int)fd);
    proc_fdvis_close((int)fd);
    /* Every typed guest descriptor has a same-number native shadow used only by legacy syscall paths. */
    close((int)fd);
}

static void exec_close_bound_cloexec_all(void) {
    uint32_t closed;
    if (g_linux_box != NULL) (void)hl_linux_fd_exec_all(g_linux_box, exec_bound_closed, NULL, &closed);
}

#include "../../host/system.h"

#if defined(__linux__)
#include <sys/prctl.h> // host PR_SET_CHILD_SUBREAPER: let the host kernel reparent orphaned guest tasks
#endif

static void exec_close_cloexec(void) {
    // Sweep only the fds that are actually OPEN, not the whole descriptor table. The daemon raises the
    // soft fd limit very high (getdtablesize() ~= 180K), so the old `for (fd=0; fd<getdtablesize())` scan
    // issued ~180K fcntl(F_GETFD) syscalls -- ~21ms -- on EVERY execve. That single loop dominated the cost
    // of an exec and made process-spawn-heavy guests (make/configure/npm/go/pip fork+exec hundreds to
    // thousands of children) appear to hang: 21ms x thousands of execs = seconds of pure descriptor
    // scanning. Host process inspection returns just the live descriptors (a couple dozen), so the sweep becomes
    // O(open fds). The real close-on-exec semantics are unchanged: every open non-engine CLOEXEC fd is
    // still closed. Fall back to a bounded linear scan only if host enumeration is unavailable.
    size_t need = 0;
    exec_close_bound_cloexec_all();
    if (!hl_host_process_fds(getpid(), NULL, 0, &need)) {
        exec_close_cloexec_scan(getdtablesize());
        return;
    }
    // Over-allocate a little: fds can be opened between the sizing call and the listing call.
    size_t cap = need <= SIZE_MAX - 32 ? need + 32 : need;
    hl_host_process_fd *fds = cap != 0 ? malloc(cap * sizeof *fds) : NULL;
    if (!fds) {
        exec_close_cloexec_scan(getdtablesize());
        return;
    }
    size_t got = 0;
    if (!hl_host_process_fds(getpid(), fds, cap, &got)) {
        free(fds);
        exec_close_cloexec_scan(getdtablesize());
        return;
    }
    // Truncated listing: more fds were open than the sized buffer could hold (they can be opened between
    // the sizing call and this one, which is why cap was over-allocated in the first place). Clamping
    // `got` here silently dropped the tail, leaving those CLOEXEC descriptors OPEN across the exec -- the
    // exact leak this sweep exists to prevent, and one that only shows up when the process is busy enough
    // to win that race. Fall back to the exhaustive scan, which cannot miss.
    if (got > cap) {
        free(fds);
        exec_close_cloexec_scan(getdtablesize());
        return;
    }
    for (size_t i = 0; i < got; i++) {
        int fd = fds[i].descriptor;
        if (exec_fd_is_bound(fd)) continue;
        if ((fds[i].flags & HL_HOST_PROCESS_FD_ENGINE_PRIVATE) != 0) continue;
        if (exec_fd_is_engine(fd)) continue;
        int fl = fcntl(fd, F_GETFD);
        if (fl >= 0 && (fl & FD_CLOEXEC)) {
            fd_reset_emul(fd); // drop hl's emulation tables for this fd so a reused number isn't misrouted
            close(fd);
        }
    }
    free(fds);
}

#include "../../core/engine_result.h"

// CLONE_VFORK suspends the calling parent thread until the child either
// commits an exec or exits. The engine implements process clones with host
// fork(), so use a private one-byte rendezvous to preserve that ordering.
// Without it, glibc may free the temporary child stack as soon as clone
// returns in the parent while the child is still using that stack to enter
// execve, producing an intermittent pre-exec SIGILL under compiler load.
static int g_vfork_release_fd = -1;

static void vfork_release_parent(void) {
    if (g_vfork_release_fd < 0) return;
    unsigned char committed = 1;
    while (write(g_vfork_release_fd, &committed, sizeof committed) < 0 && errno == EINTR) {}
    close(g_vfork_release_fd);
    g_vfork_release_fd = -1;
}

// ---- fork child-side engine hooks (shared by clone/case-220 and clone3/case-435) -----------------
// Everything the CHILD must reset before it re-enters guest code. Factored so the two fork sites can
// never drift (clone3 was missing the W^X re-assert and the DIR*-cache drop).

static void fork_child_hooks(struct cpu *c) {
#ifdef G_SOFT_STATE_RESET
    G_SOFT_STATE_RESET(c);
#endif
    hl_engine_child_result_after_fork();
    atomic_flag_clear_explicit(&g_bus_lock, memory_order_release);
    // Re-assert MAP_JIT execute mode: the per-thread W^X/APRR state isn't reliable across fork(),
    // so the child's first run_block can instruction-abort fetching from the (non-executable) code
    // cache -> the intermittent fork+exec SIGBUS. The host end-write gate re-asserts RX execution.
    // (No-op under the dual map, which never toggles W^X.)
    if (!jit_wprot(1)) {
        c->exit_code = 70;
        c->exited = 1;
        return;
    }
    install_host_sigaltstack(); // the altstack registration doesn't survive fork on Apple Silicon --
                                // re-arm it (COW-inherited region) so the child can still take a stack-overflow
                                // guard fault (host SP == guest SP) instead of double-faulting into SIGILL.
    if (!jit_after_fork()) {
        c->exit_code = 70;
        c->exited = 1;
        return;
    } // dual map: re-alias RX from the child's COW RW pages at the same VA (~1us; keeps
      // every inherited translation valid) -- or, threaded parent, rebuild a fresh cache
#ifdef PCACHE_FORK_HOOK
    PCACHE_FORK_HOOK; // drop inherited reloc records + apply the frontend's fork-epoch save policy
#endif
    G_SHADOW_RESET(c); // §B: child's pre-fork host_rets crossed run_block -> drop, use IBTC
    // Only when jit_after_fork REBUILT the cache at a fresh VA (threaded parent) is every cached body
    // pointer stale: it zeroed the shared g_map + g_ibtc, but the x86-only 2-way g_xibtc it cannot see
    // must ALSO be dropped -- else the child's first indirect branch resolves a stale body into the freed
    // parent RX alias -> SIGSEGV (the same class the execve path documents below). On the preserved-arena
    // path (single-threaded parent, or the MAP_JIT fallback) the cache VA and content are unchanged,
    // so the inherited g_xibtc stays valid and is kept warm.
    if (g_dualmap && !g_fork_preserved) G_SHADOW_CLEAR(c);
    // S2: invalidate inherited path/metadata caches so the child cannot serve an
    // entry populated before the filesystem diverged.
    hl_fdcache_reset();
    g_ndirs = 0;                 // the getdents DIR* cache is the PARENT's -- closedir'ing inherited handles
                                 // (on the child's close) crashes; drop it so the child re-fdopendir's fresh
    kqueue_rebuild_after_fork(); // macOS kqueue() fds (epoll/timerfd/inotify) don't survive fork ->
                                 // rebuild them so the child doesn't EBADF on its inherited event fds
                                 // (also reinits g_ep_mtx, inherited-locked if a peer forked mid-epoll)
    thread_after_fork();         // reset process-private thread/futex locks a dead peer may have held at fork
    sysv_after_fork();           // reset the SysV-shm lock (same fork-unsafe-mutex class)
    eventfd_after_fork();        // reset the eventfd counter+pipe lock (fork-unsafe-mutex class)
    anon_after_fork();           // reset the private-anon registry lock (fork-unsafe-mutex class); must precede
                                 // wipefork/dontfork_apply_child below, which mutate the registry in this child
    ts_after_fork();             // drop the inherited task-state slot cache so the child re-claims its own
    container_pid_after_fork();  // child has a new host pid -> drop the cached getpid() so it re-reads its own
    poslk_after_fork();          // re-cache pid; child inherits NONE of the parent's fcntl record locks
    flock_broker_after_fork();   // flock ownership is OFD-scoped and IS inherited across fork
    proc_reg_after_fork();       // publish the fork child in /proc and stop it inheriting the parent's registry path
    acct_after_fork();           // claim this child's OWN cgroup accounting slot (new host pid, one task)
    wipefork_apply_child();      // MADV_WIPEONFORK: zero-fill the ranges the guest marked wipe-on-fork
    dontfork_apply_child();      // MADV_DONTFORK: unmap the ranges the guest marked not-inherited-on-fork
    hl_gmap_lock_reset();        // mlock(2): memory locks are NOT inherited across fork -> child starts unlocked
}

typedef struct bound_fork_state {
    hl_linux_fork_plan plan;
    hl_linux_watch_fork_plan watch_plan;
    int watch_prepared;
    int private_prepared;
    struct fdvis_fork_plan fdvis_plan;
    int fdvis_prepared;
    int seq_prepared;
} bound_fork_state;

static int bound_fork_prepare(bound_fork_state *state) {
    hl_status status;
    memset(state, 0, sizeof(*state));
    if (g_linux_box == NULL) {
        int private_status = hl_host_process_fd_private_fork_prepare();
        if (private_status != 0) return private_status;
        state->private_prepared = 1;
        int fdvis_status = proc_fdvis_fork_prepare(&state->fdvis_plan);
        if (fdvis_status != 0) {
            (void)hl_host_process_fd_private_fork_complete(0);
            state->private_prepared = 0;
            return fdvis_status == -ENOSPC ? -ENOMEM : fdvis_status;
        }
        state->fdvis_prepared = 1;
        seq_ref_fork_prepare();
        state->seq_prepared = 1;
        return 0;
    }
    state->watch_plan.capacity = bound_mapping_watch_capacity();
    // Records are populated only up to plan->count (each written via a fully-designated compound literal) and
    // read only over [0,count); the tail is never inspected, so the worst-case-sized buffer needs no zero-fill.
    // malloc avoids the per-fork memset of a capacity-sized (ofd_capacity) block that calloc would perform.
    state->watch_plan.records = state->watch_plan.capacity == 0
                                    ? NULL
                                    : malloc((size_t)state->watch_plan.capacity * sizeof(*state->watch_plan.records));
    if (state->watch_plan.capacity != 0 && state->watch_plan.records == NULL) { return -ENOMEM; }
    if (bound_mapping_fork_prepare(&state->watch_plan) != 0) {
        free(state->watch_plan.records);
        state->watch_plan.records = NULL;
        return -EIO;
    }
    state->watch_prepared = 1;
    state->plan.abi = HL_LINUX_ABI_VERSION;
    state->plan.size = sizeof(state->plan);
    state->plan.capacity = g_linux_box->ofd_capacity;
    // hl_linux_abi_fork_prepare writes each record with a fully-designated compound literal and all consumers
    // iterate only [0,count); the unused tail is never read, so no zero-fill is required. malloc avoids the
    // per-fork memset of the capacity-sized (ofd_capacity) records block that calloc would perform.
    state->plan.records = malloc((size_t)state->plan.capacity * sizeof(*state->plan.records));
    if (state->plan.records == NULL) {
        (void)bound_mapping_fork_complete(&state->watch_plan, 0);
        state->watch_prepared = 0;
        free(state->watch_plan.records);
        state->watch_plan.records = NULL;
        return -ENOMEM;
    }
    status = hl_linux_abi_fork_prepare(g_linux_box, &state->plan);
    if (status != HL_STATUS_OK) {
        (void)bound_mapping_fork_complete(&state->watch_plan, 0);
        state->watch_prepared = 0;
        free(state->plan.records);
        state->plan.records = NULL;
        free(state->watch_plan.records);
        state->watch_plan.records = NULL;
        return status == HL_STATUS_BUSY ? -EAGAIN : status == HL_STATUS_OUT_OF_MEMORY ? -ENOMEM : -EIO;
    }
    {
        int private_status = hl_host_process_fd_private_fork_prepare();
        if (private_status != 0) {
            (void)hl_linux_abi_fork_parent(g_linux_box, &state->plan);
            (void)bound_mapping_fork_complete(&state->watch_plan, 0);
            state->watch_prepared = 0;
            free(state->plan.records);
            state->plan.records = NULL;
            free(state->watch_plan.records);
            state->watch_plan.records = NULL;
            return private_status;
        }
    }
    state->private_prepared = 1;
    {
        int fdvis_status = proc_fdvis_fork_prepare(&state->fdvis_plan);
        if (fdvis_status != 0) {
            (void)hl_host_process_fd_private_fork_complete(0);
            state->private_prepared = 0;
            (void)hl_linux_abi_fork_parent(g_linux_box, &state->plan);
            (void)bound_mapping_fork_complete(&state->watch_plan, 0);
            state->watch_prepared = 0;
            free(state->plan.records);
            state->plan.records = NULL;
            free(state->watch_plan.records);
            state->watch_plan.records = NULL;
            return fdvis_status == -ENOSPC ? -ENOMEM : fdvis_status;
        }
    }
    state->fdvis_prepared = 1;
    seq_ref_fork_prepare();
    state->seq_prepared = 1;
    return 0;
}

static int bound_fork_complete(bound_fork_state *state, int child, int child_pid) {
    hl_status status;
    if (state->seq_prepared && child_pid < 0) seq_ref_fork_cancel();
    if (state->fdvis_prepared) {
        if (child_pid > 0)
            proc_fdvis_after_fork(&state->fdvis_plan, child_pid, child);
        else
            proc_fdvis_fork_cancel(&state->fdvis_plan);
        free(state->fdvis_plan.entries);
        state->fdvis_plan.entries = NULL;
    }
    int private_status = state->private_prepared ? hl_host_process_fd_private_fork_complete(child) : 0;
    if (g_linux_box == NULL) return private_status;
    status = child ? hl_linux_abi_fork_child(g_linux_box, &state->plan)
                   : hl_linux_abi_fork_parent(g_linux_box, &state->plan);
    if (state->watch_prepared && bound_mapping_fork_complete(&state->watch_plan, child) != 0 && status == HL_STATUS_OK)
        status = HL_STATUS_PLATFORM_FAILURE;
    if (private_status != 0 && status == HL_STATUS_OK) status = HL_STATUS_OUT_OF_MEMORY;
    free(state->plan.records);
    state->plan.records = NULL;
    free(state->watch_plan.records);
    state->watch_plan.records = NULL;
    return status == HL_STATUS_OK ? 0 : -EIO;
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
static int g_dumpable = 1;                 // PR_SET/GET_DUMPABLE
static int g_pdeathsig;                    // PR_SET/GET_PDEATHSIG
static unsigned long g_timerslack = 50000; // PR_SET/GET_TIMERSLACK (ns); Linux default is 50us
static int g_thp_disable;                  // PR_SET/GET_THP_DISABLE (per-process transparent-hugepage opt-out)
static int g_subreaper;                    // PR_SET/GET_CHILD_SUBREAPER (this process is a reaper for orphans)
static int g_mce_kill = 2; // PR_MCE_KILL/PR_MCE_KILL_GET machine-check policy: LATE=0, EARLY=1, DEFAULT=2
// The process EFFECTIVE capability set. The container starts as full root (all caps); we don't model
// per-capability ENFORCEMENT in general, but we DO track what capset(2) leaves in the effective set so the
// few prctl options the kernel gates on a specific capability (PR_SET_SECUREBITS / PR_CAPBSET_DROP need
// CAP_SETPCAP) return -EPERM after that cap has been dropped -- exactly as LTP prctl02 (which drops
// CAP_SETPCAP via libcap before those subtests) expects. g_cap_eff/g_cap_bnd are DEFINED in
// container/state.c (default = the 14-cap docker set HL_CAP_DEFAULT, which INCLUDES CAP_SETPCAP) so the
// /proc/self/status builder and these handlers share one source of truth. capset() narrows the effective set.
#define CAP_SETPCAP 8
#define CAP_SYS_RESOURCE 24
// personality(2) persona (lsys-personality): query with 0xffffffff, set returns the previous value.
static unsigned g_persona;

// ===================== sched_setscheduler / sched_*param family =====================
// macOS has no Linux scheduling policies, so hl does not actually change host scheduling; it validates the
// arguments exactly as the Linux kernel does (policy/priority/pid/pointer -> EINVAL/ESRCH/EFAULT) and keeps
// a per-process record of the requested policy+priority so sched_getscheduler/sched_getparam round-trip.
static int g_sched_policy = 0; // SCHED_OTHER
static int g_sched_prio;       // sched_priority last set
#define HL_SCHED_RESET_ON_FORK 0x40000000

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
    case 6:
        *lo = 0;
        *hi = 0;
        return 0; // SCHED_OTHER / SCHED_BATCH / SCHED_IDLE / SCHED_DEADLINE
    default: return -1;
    }
}

// Does guest pid `gpid` name a live task? pid 0 (and the container init pid) is the caller itself; init pid 1
// maps to its real host pid. A guest THREAD tid (glibc's pthread_getaffinity_np passes pd->tid, which the
// JVM/Go/etc. do heavily) is a engine-internal id (g_next_tid, base 1000) that is NOT a host pid -- probing it
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

static void translation_log_summary(void *context, uint64_t translations, uint64_t translation_ns) {
    (void)translation_ns;
    HL_LOGF((hl_log_context *)context, HL_LOG_TAG_TRANSLATE, "blocks=%llu", (unsigned long long)translations);
}

static int credential_publish_or_fault(struct cpu *c) {
    if (hl_target_credentials_publish(c)) return 1;
    c->exited = 1;
    c->exit_code = 127;
    return 0;
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
        uint32_t header[2];
        // Import through the logical-VMA resolver: the header may live in canonical storage rather than at
        // its Linux-visible address.
        if (!a0 || guest_copy_from(header, a0, sizeof header) != sizeof header) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint32_t ver = header[0];
        int u32s; // number of __user_cap_data_struct the version spans
        switch (ver) {
        case 0x19980330: u32s = 1; break; // _LINUX_CAPABILITY_VERSION_1 (1 u32 mask)
        case 0x20071026:                  // _LINUX_CAPABILITY_VERSION_2 (deprecated)
        case 0x20080522: u32s = 2; break; // _LINUX_CAPABILITY_VERSION_3 (2 u32 masks, 64 caps)
        default:
            // kernel cap_validate_magic: rewrite header->version to its preferred (v3). A pure version
            // probe (data==NULL) then succeeds; otherwise it is EINVAL (LTP capget02 "bad version" +
            // the libcap-ng negotiation probe). The rewrite is what the test asserts on afterwards.
            header[0] = 0x20080522;
            if (guest_copy_to(a0, header, sizeof(uint32_t)) != sizeof(uint32_t)) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            G_RET(c) = a1 ? (uint64_t)(-EINVAL) : 0;
            goto cap_done;
        }
        // header->pid selects the target task: <0 -> EINVAL, a dead pid -> ESRCH (LTP capget02
        // "bad pid"/"unused pid"). 0/self/our own tid/pid resolve to this process (capget01 uses getpid()).
        int tpid = (int)header[1];
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
            uint32_t d[6] = {0};
            for (int i = 0; i < u32s; i++) {
                uint32_t eff = (i == 0) ? (uint32_t)g_cap_eff : (uint32_t)(g_cap_eff >> 32);
                // permitted = the docker default 14-cap set (HL_CAP_DEFAULT), NOT a blanket all-ones: a
                // default `docker run` root container has CapPrm=00000000a80425fb, matching /proc/self/status
                // exactly. The old 0xffffffff over-reported caps (e.g. CAP_SYS_ADMIN) the container lacks.
                uint32_t prm = (i == 0) ? (uint32_t)HL_CAP_DEFAULT : (uint32_t)(HL_CAP_DEFAULT >> 32);
                d[i * 3 + 0] = eff; // effective: the guest's live effective set (respects drops)
                d[i * 3 + 1] = prm; // permitted: the docker default bounding/permitted set
                d[i * 3 + 2] = 0;   // inheritable: empty (Docker default)
            }
            size_t bytes = (size_t)u32s * 12;
            if (guest_copy_to(a1, d, bytes) != (ssize_t)bytes) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
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
        uint32_t header[2];
        if (!a0 || guest_copy_from(header, a0, sizeof header) != sizeof header) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint32_t ver = header[0];
        if (ver != 0x19980330 && ver != 0x20071026 && ver != 0x20080522) {
            uint32_t preferred = 0x20080522;
            if (guest_copy_to(a0, &preferred, sizeof preferred) != sizeof preferred)
                G_RET(c) = (uint64_t)(-EFAULT);
            else
                G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        // a capset re-raises EFFECTIVE caps from the PERMITTED set (the only bits it may set). After a
        // KEEPCAPS uid drop this is how setpriv restores effective CAP_SETGID so its following setresgid works.
        cred_init();
        g_cap_setid_eff = g_cap_setid_perm;
        // Track the effective set the guest just asked for, so a capability-gated prctl (PR_SET_SECUREBITS /
        // PR_CAPBSET_DROP) reflects a dropped CAP_SETPCAP. datap is {effective,permitted,inheritable}[u32s];
        // effective words are at data[i*3+0]. v1 spans the low 32 caps, v3 the full 64.
        int u32s = (ver == 0x19980330u) ? 1 : 2;
        uint32_t d[6];
        size_t bytes = (size_t)u32s * 12;
        if (!a1 || guest_copy_from(d, a1, bytes) != (ssize_t)bytes) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint64_t eff = d[0];
        if (u32s == 2) eff |= (uint64_t)d[3] << 32;
        g_cap_eff = eff;
        G_RET(c) = 0;
        break;
    }
    // chroot(path): re-root the guest WITHIN the rootfs jail. Resolve the target through the active jail to
    // its host backing -- this validates it exists as a directory inside the rootfs and can NEVER name a
    // host path -- then record it as the new chroot prefix. Subsequent absolute guest paths are walked
    // under this prefix yet stay confined to g_root_fd, so the guest cannot escape to the real host fs.
    case 51: {
        char guest_path[4200], gabs[4200];
        int imported = guest_copy_string(guest_path, sizeof guest_path, a0);
        if (imported < 0) {
            G_RET(c) = (uint64_t)(int64_t)imported;
            break;
        }
        abs_guest(-100, guest_path, gabs, sizeof gabs); // (AT_FDCWD, path) -> guest-view abs
        if (g_rootfs && synth_proc_fd_dir_is(gabs)) {
            char isolated[128], backing[4200];
            snprintf(isolated, sizeof isolated, "/.hl-proc-chroot-%d", (int)getpid());
            confine_in(g_rootfs_canon, g_rootfs_canon_len, isolated, backing, sizeof backing, 1);
            if (mkdir(backing, 0700) != 0 && errno != EEXIST) {
                G_RET(c) = (uint64_t)(int64_t)(-errno);
                break;
            }
            snprintf(g_chroot, sizeof g_chroot, "%s", isolated);
            snprintf(g_cwd, sizeof g_cwd, "/");
            hl_fdcache_reset();
            G_RET(c) = 0;
            break;
        }
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
        hl_fdcache_reset(); // drop cached guest->host path mappings -- they predate the re-root
        G_RET(c) = 0;
        break;
    }
    case 93:
        c->exited = 1;
        // Linux exposes only the low eight bits of an exit status to waiters.
        // Preserve that contract on the translated `exit` path too: unlike the
        // `exit_group` path below, this unwinds through the engine instead of
        // reaching the host `_exit`, so the kernel cannot normalize it for us.
        c->exit_code = (int)(a0 & 0xffu);
        // exit: end THIS thread
        break;
    // exit_group: end the whole process
    case 94:
        HL_LOGF(&g_jit_log, HL_LOG_TAG_NETWORK, "exit_group pid=%d code=%d", (int)getpid(), (int)a0);
        hl_dispatch_profile_report(&g_dispatch_profile, &g_jit_log, translation_log_summary);
        if (g_prof)
            fprintf(stderr,
                    "[prof] crossings=%llu syscalls=%llu ibtc_miss=%llu branch_cross=%llu translations=%llu lse=%llu "
                    "wx_toggles=%llu dualmap=%d xlate_ms=%.3f service_ms=%.3f mtibtc=%d mtfill=%llu "
                    "fwake_fast=%llu fwake_slow=%llu fwait=%llu soft_sample_shift=6 soft_sampled_sites=%llu "
                    "soft_hull_direct_sampled=%llu soft_cached_hit_sampled=%llu soft_miss=%llu soft_span=%llu "
                    "soft_bounce_prepare=%llu soft_bounce_commit=%llu smc_queued=%llu smc_commit=%llu\n",
                    (unsigned long long)g_prof_cross, (unsigned long long)g_prof_sys, (unsigned long long)g_prof_miss,
                    (unsigned long long)(g_prof_cross - g_prof_sys - g_prof_miss), (unsigned long long)g_prof_xlate,
                    (unsigned long long)g_lse_n, (unsigned long long)g_wx_toggles, g_dualmap, g_xlate_ns / 1e6,
                    g_service_ns / 1e6, g_mtibtc, (unsigned long long)g_mtfill, (unsigned long long)g_futex_wake_fast,
                    (unsigned long long)g_futex_wake_slow, (unsigned long long)g_futex_wait_n,
                    (unsigned long long)g_prof_soft_sites_sampled, (unsigned long long)g_prof_soft_hull_sampled,
                    (unsigned long long)g_prof_soft_cached_sampled, (unsigned long long)g_prof_soft_miss,
                    (unsigned long long)g_prof_soft_span, (unsigned long long)g_prof_soft_bounce_prepare,
                    (unsigned long long)g_prof_soft_bounce_commit, (unsigned long long)g_prof_smc_queued,
                    (unsigned long long)g_prof_smc_commit);
        // A3: §B shadow-return coverage. hit-rate = shret_hit / (shret_hit + shret_fb). bl_shadow /
        // bl_leaf show how the depth-gate split call sites at translate time. PROF-only (keep dark).
        if (0) {
            unsigned long long h = (unsigned long long)g_prof_shret_hit, f = (unsigned long long)g_prof_shret_fb;
            double hr = (h + f) ? 100.0 * (double)h / (double)(h + f) : 0.0;
            fprintf(
                stderr,
                "[prof] shadow_push=%llu shret_hit=%llu shret_fb=%llu hit_rate=%.1f%% bl_shadow=%llu bl_leaf=%llu\n",
                (unsigned long long)g_prof_shpush, h, f, hr, (unsigned long long)g_prof_bl_shadow,
                (unsigned long long)g_prof_bl_leaf);
        }
        if (g_noexit) { // W3D fork-server prewarm: don't kill the resident parent; unwind run_guest instead
            c->exited = 1;
            c->exit_code = (int)(a0 & 0xffu);
            break;
        }
#ifdef PCACHE_SAVE_HOOK
        PCACHE_SAVE_HOOK; // persist the translated arena before one-shot exit when HL_PCACHE is active
#endif
        futex_robust_exit(c);   // robust mutexes still held by the calling thread -> OWNER_DIED + wake waiters
        launch_reg_terminate_peers(); // PID-namespace init exit kills every launch-owned descendant, even setsid peers
        udp_ref_process_exit(); // unlink AF_UNIX rendezvous inodes whose last owner is this exiting process
        acct_proc_leave();      // release this process's cgroup accounting slot (_exit bypasses atexit)
        proc_reg_unlink();      // drop our /proc process-table entry (_exit bypasses the atexit handler)
        proc_fdvis_cleanup();   // retire typed logical-fd identities (_exit bypasses the atexit handler)
        hl_host_process_fd_private_cleanup(); // retire provider-private descriptors for this process identity
        poslk_on_exit();                      // release this process's in-engine fcntl advisory locks
        sysv_on_exit();                       // apply SEM_UNDO + GC this container's SysV objects (_exit skips atexit)
        hl_engine_child_result_publish((int32_t)a0, HL_STATUS_OK, 0);
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
    case 268: G_RET(c) = ((int)a0 < 0) ? (uint64_t)(int64_t)(-EBADF) : 0; break;
    // futex
    case 98: { // futex(uaddr, op, val, timeout|nr_wake2=a3, uaddr2=a4, val3=a5)
        const unsigned raw_operation = (unsigned)a1;
        const unsigned known_operation_bits = 0x7fu | 0x80u /* PRIVATE */ | 0x100u /* CLOCK_REALTIME */;
        if (raw_operation & ~known_operation_bits) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        int operation = (int)raw_operation & 0x7f;
        // Linux accepts FUTEX_CLOCK_REALTIME only for the absolute-time
        // operations.  In particular WAIT|CLOCK_REALTIME is ENOSYS; silently
        // dropping the flag turns it into a different wait operation.
        if ((raw_operation & 0x100u) != 0 && operation != 9 && operation != 11 && operation != 13) {
            G_RET(c) = (uint64_t)(int64_t)(-ENOSYS);
            break;
        }
        void *primary = NULL, *secondary = NULL, *timeout = (void *)(uintptr_t)a3;
        hl_logical_vma_pin primary_pin = {0}, secondary_pin = {0}, timeout_pin = {0};
        if (a0 & 3) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        uint32_t primary_access = HL_LOGICAL_VMA_READ;
        if (operation == 6 || operation == 7 || operation == 8 || operation == 13)
            primary_access |= HL_LOGICAL_VMA_WRITE;
        if (guest_atomic_address(a0, sizeof(uint32_t), primary_access, &primary, &primary_pin) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a4 && (operation == 3 || operation == 4 || operation == 5 || operation == 11 || operation == 12)) {
            if (a4 & 3) {
                hl_logical_vma_unpin(&primary_pin);
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            uint32_t secondary_access = HL_LOGICAL_VMA_READ;
            if (operation == 5 || operation == 11 || operation == 12) secondary_access |= HL_LOGICAL_VMA_WRITE;
            if (guest_atomic_address(a4, sizeof(uint32_t), secondary_access, &secondary, &secondary_pin) < 0) {
                hl_logical_vma_unpin(&primary_pin);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        } else {
            secondary = (void *)(uintptr_t)a4;
        }
        if (a3 && (operation == 0 || operation == 6 || operation == 9 || operation == 11 || operation == 13) &&
            guest_atomic_address(a3, sizeof(struct timespec), HL_LOGICAL_VMA_READ, &timeout, &timeout_pin) < 0) {
            hl_logical_vma_unpin(&primary_pin);
            hl_logical_vma_unpin(&secondary_pin);
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // a3 is a timespec* for waits and a wake count for WAKE_OP; operation selects its interpretation.
        int is_private = (raw_operation & 0x80u) != 0;
        /*
         * futex_op's bucket/key helpers canonicalize addresses which belong to
         * MAP_SHARED file mappings. FUTEX_PRIVATE_FLAG must instead remain
         * keyed by this process's virtual address: two aliases of one memfd
         * are distinct private futexes. Move private identity into the
         * non-canonical host half so the shared-object registry cannot fold it
         * back to (dev,inode,offset), while preserving a stable one-to-one key.
         */
        const uintptr_t private_tag = UINT64_C(0x8000000000000000);
        const void *primary_key = is_private ? (const void *)(uintptr_t)(a0 ^ private_tag) : primary;
        const void *secondary_key = is_private ? (const void *)(uintptr_t)(a4 ^ private_tag) : secondary;
        G_RET(c) = (uint64_t)futex_op(c, primary, primary_key, operation, is_private, (int)a2, timeout, (int)a3,
                                      secondary, secondary_key, (uint32_t)a5);
        hl_logical_vma_unpin(&timeout_pin);
        hl_logical_vma_unpin(&secondary_pin);
        hl_logical_vma_unpin(&primary_pin);
        break;
    }
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
        size_t copy = n < 128 ? n : 128;
        uint8_t mask[128] = {0};
        // Linux get_user_cpu_mask() copy_from_user()s min(len, cpumask_size) bytes FIRST -> a bad mask pointer
        // (with len>0) is -EFAULT before anything else. The old handler read the guest mask straight through in
        // hl_linux_affinity_set(), so an unmapped pointer SEGV'd the engine instead of returning EFAULT. len==0
        // copies nothing (no fault) and falls through to the empty-mask -EINVAL below.
        if (copy && guest_copy_from(mask, a2, copy) != (ssize_t)copy) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Then, like sched_getaffinity, the target task must exist: find_task_by_vpid() -> -ESRCH for a pid that
        // names no live task. The old handler skipped this and "succeeded" (returned 0) for any pid.
        if (sched_pid_live((int)(int32_t)a0) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (!hl_linux_affinity_set(&g_affinity, mask, copy, linux_online_cpus())) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
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
        if ((n & (sizeof(unsigned long) - 1)) || n * 8 < (size_t)linux_online_cpus()) {
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
        // NULL is not exempt: the size check above already guarantees n >= sizeof(long), so Linux always
        // copies out and sched_getaffinity(0, sizeof(mask), NULL) is -EFAULT. The old `a2 &&` guard made a
        // NULL mask "succeed" (returning the byte count) while writing nothing.
        size_t copy = n < 128 ? n : 128;
        if (copy && guest_accessible_prefix(a2, copy, HL_LOGICAL_VMA_WRITE) != copy) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (copy && guest_copy_to(a2, hl_linux_affinity_get(&g_affinity, linux_online_cpus()), copy) != (ssize_t)copy) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        // Return the number of bytes the mask spans (glibc zeroes the remainder); 8 covers <=64 CPUs.
        G_RET(c) = copy < 8 ? (uint64_t)copy : 8;
        break;
    }
    // sched_yield
    case 124: G_RET(c) = 0; break;
    // ---- sched_setscheduler / sched_*param arg-validation family (LTP sched_*01..03). hl has no real
    // Linux scheduling classes, so these validate exactly like the kernel and record the requested
    // policy/priority for round-trip reads; the errno ORDER matches the kernel line-for-line.
    // sched_setparam(pid, param)
    case 118: {
        int pid = (int)a0;
        if (!a1 || pid < 0) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        } // do_sched_setscheduler: !param||pid<0
        int prio;
        if (guest_copy_from(&prio, a1, sizeof prio) != sizeof prio) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
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
        int prio;
        if (guest_copy_from(&prio, a2, sizeof prio) != sizeof prio) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        } // copy_from_user(param)
        if (sched_pid_live(pid) < 0) {
            G_RET(c) = (uint64_t)(-ESRCH);
            break;
        } // find_process_by_pid
        int base = policy & ~HL_SCHED_RESET_ON_FORK, lo, hi;
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
        if (guest_copy_to(a1, &g_sched_prio, sizeof g_sched_prio) != sizeof g_sched_prio) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
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
        const uint64_t interval[2] = {0, 100000000};
        if (guest_copy_to(a1, interval, sizeof interval) != sizeof interval) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
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
        if (!credential_publish_or_fault(c)) break;
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
        if (!credential_publish_or_fault(c)) break;
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
        if (!credential_publish_or_fault(c)) break;
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
        if (!credential_publish_or_fault(c)) break;
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
        if (!credential_publish_or_fault(c)) break;
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
        if (!credential_publish_or_fault(c)) break;
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
        if (!credential_publish_or_fault(c)) break;
        G_RET(c) = (uint64_t)(uint32_t)prev;
        break;
    }
    case 152: {
        cred_init();
        int prev = newfile_gid(), g = (int)a0;
        if (g != -1 && gid_permitted(g)) g_fsgid_ovr = (g == g_egid) ? -1 : g;
        if (!credential_publish_or_fault(c)) break;
        G_RET(c) = (uint64_t)(uint32_t)prev;
        break;
    }
    case 148: {
        // getresuid(r,e,s) -- report the overlay so a runtime drop is observed (apt verifies all three).
        // Linux faults the whole call if any output pointer is NULL/unwritable (EFAULT), writing none.
        cred_init();
        if (!a0 || !a1 || !a2 || guest_accessible_prefix(a0, 4, HL_LOGICAL_VMA_WRITE) != 4 ||
            guest_accessible_prefix(a1, 4, HL_LOGICAL_VMA_WRITE) != 4 ||
            guest_accessible_prefix(a2, 4, HL_LOGICAL_VMA_WRITE) != 4) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint32_t ids[3] = {(uint32_t)g_ruid, (uint32_t)g_euid, (uint32_t)g_suid};
        (void)guest_copy_to(a0, ids, 4);
        (void)guest_copy_to(a1, ids + 1, 4);
        (void)guest_copy_to(a2, ids + 2, 4);
        G_RET(c) = 0;
        break;
    }
    case 150: {
        // getresgid(r,e,s) -- report the overlay (see getresuid above). NULL/unwritable pointer -> EFAULT.
        cred_init();
        if (!a0 || !a1 || !a2 || guest_accessible_prefix(a0, 4, HL_LOGICAL_VMA_WRITE) != 4 ||
            guest_accessible_prefix(a1, 4, HL_LOGICAL_VMA_WRITE) != 4 ||
            guest_accessible_prefix(a2, 4, HL_LOGICAL_VMA_WRITE) != 4) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        uint32_t ids[3] = {(uint32_t)g_rgid, (uint32_t)g_egid, (uint32_t)g_sgid};
        (void)guest_copy_to(a0, ids, 4);
        (void)guest_copy_to(a1, ids + 1, 4);
        (void)guest_copy_to(a2, ids + 2, 4);
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
                size_t bytes = (size_t)cnt * sizeof(gid_t);
                if (guest_copy_to(a1, g_groups, bytes) != (ssize_t)bytes) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
            }
            G_RET(c) = (uint64_t)cnt;
            break;
        }
        if (g_gid >= 0) {
            // getgroups -> [effective gid]. Tracking the overlay's egid means apt's drop to _apt's group
            // is reflected here too (it setgroups(1,&_apt_gid) right before switching).
            gid_t egid = (gid_t)cred_egid();
            if ((int)a0 >= 1 && a1 && guest_copy_to(a1, &egid, sizeof egid) != sizeof egid) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
            G_RET(c) = 1;
            break;
        }
        int count = (int)a0;
        gid_t *groups = count > 0 ? calloc((size_t)count, sizeof *groups) : NULL;
        if (count > 0 && !groups) {
            G_RET(c) = (uint64_t)(-ENOMEM);
            break;
        }
        int r = getgroups(count, groups);
        if (r >= 0 && count > 0 && a1 &&
            guest_copy_to(a1, groups, (size_t)r * sizeof *groups) != (ssize_t)((size_t)r * sizeof *groups)) {
            free(groups);
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        free(groups);
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
        if (ng < 0 || ng > HL_NGROUPS_MAX) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (ng > 0) {
            if (!a1 ||
                guest_copy_from(g_groups, a1, (size_t)ng * sizeof(gid_t)) != (ssize_t)((size_t)ng * sizeof(gid_t))) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
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
        // The 144-byte struct rusage is written directly by the engine (not via a host syscall), so a
        // bad/unmapped pointer must return -EFAULT here rather than fault the engine (access_ok).
        // NULL is NOT exempt: Linux always copy_to_user()s the buffer, so getrusage(RUSAGE_SELF, NULL)
        // is -EFAULT. The old `if (a1)` guard silently skipped the copy-out and returned success.
        if (guest_accessible_prefix(a1, 144, HL_LOGICAL_VMA_WRITE) != 144) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint8_t linux_ru[144] = {0};
        if (getrusage(who, &ru) == 0) rusage_to_linux(linux_ru, &ru);
        if (guest_copy_to(a1, linux_ru, sizeof linux_ru) != sizeof linux_ru) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        G_RET(c) = 0;
        break;
    }
    // prctl(option,...)
    case 167: {
        if ((int)a0 == 15) {
            // PR_SET_NAME: kernel copies up to 16 bytes from arg2 -> -EFAULT on an unreadable pointer
            // (LTP prctl02 PR_SET_NAME/bad_addr). Validate before the deref.
            char name[16] = {0};
            int name_fault = 0;
            for (size_t i = 0; i < sizeof name; ++i) {
                if (guest_copy_from(name + i, a1 + i, 1) != 1) {
                    name_fault = 1;
                    break;
                }
                if (!name[i]) break;
            }
            if (name_fault) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            name[15] = 0;
            snprintf(g_procname, sizeof g_procname, "%.15s", name);
            // Leader only: keep /proc/self/{comm,status,stat} in sync. A worker renames just its own task.
            set_guest_comm_name(g_procname, c->tid == 0);
            G_RET(c) = 0;
            break;
        } // PR_SET_NAME
        if ((int)a0 == 16) {
            char name[16] = {0};
            // Never set on this thread -> report the process comm, which is what a fresh task inherits.
            if (g_procname[0])
                snprintf(name, sizeof name, "%s", g_procname);
            else
                proc_comm(name, sizeof name);
            if (guest_copy_to(a1, name, sizeof name) != sizeof name) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
            G_RET(c) = 0;
            break;
        } // PR_GET_NAME
        // PR_SET_TIMERSLACK(29)/PR_GET_TIMERSLACK(30): the per-process timer slack (ns) round-trips. SET with
        // arg2==0 resets to the default (Linux copies the process's default_timer_slack_ns); GET returns the
        // current value as the syscall return. Previously both fell through to the generic no-op/EINVAL
        // switch, so GET never reported what SET stored.
        if ((int)a0 == 29) {
            g_timerslack = a1 ? (unsigned long)a1 : 50000UL;
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 30) {
            G_RET(c) = (uint64_t)g_timerslack;
            break;
        }
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
            // Forward to the host prctl on Linux: each guest process is a real host process whose parent is
            // the guest parent's host process, so the host kernel delivers the parent-death signal when that
            // parent dies -- exactly like the PR_SET_CHILD_SUBREAPER forward below. Without this the guest's
            // pdeathsig never fired and a child blocked in sigwait() hung forever.
#if defined(__linux__) && defined(PR_SET_PDEATHSIG)
            (void)prctl(PR_SET_PDEATHSIG, (unsigned long)(int)a1, 0UL, 0UL, 0UL);
#endif
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 2) {
            if (guest_copy_to(a1, &g_pdeathsig, sizeof g_pdeathsig) != sizeof g_pdeathsig) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
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
        // process-tree feature hl's 1:1 host-fork model does not implement (an orphaned guest grandchild is
        // reparented by the host kernel, not routed back to the guest subreaper), so prctl03's reparent/
        // SIGCHLD/wait subtests are a known process-model gap, out of this syscall layer's scope.
        if ((int)a0 == 36) {
            g_subreaper = (a1 != 0);
            // hl runs each guest task as a real host process, so orphan reparenting is a host-kernel
            // decision. Forward the flag to the host prctl on Linux: the host kernel then reparents an
            // orphaned guest grandchild onto THIS engine process (the marked subreaper) instead of host
            // init, so the guest's own waitpid(-1) harvests it -- matching PR_SET_CHILD_SUBREAPER. The
            // guest-visible flag still round-trips via g_subreaper for PR_GET below.
#if defined(__linux__) && defined(PR_SET_CHILD_SUBREAPER)
            (void)prctl(PR_SET_CHILD_SUBREAPER, a1 != 0 ? 1UL : 0UL, 0UL, 0UL, 0UL);
#endif
            G_RET(c) = 0;
            break;
        }
        if ((int)a0 == 37) {
            if (guest_copy_to(a1, &g_subreaper, sizeof g_subreaper) != sizeof g_subreaper) {
                G_RET(c) = (uint64_t)(-EFAULT);
                break;
            }
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
                // PR_CAP_AMBIENT_RAISE(2) requires the cap be present in BOTH the permitted and the
                // inheritable set (kernel/sys.c: !pP || !pI || issecure(NO_CAP_AMBIENT_RAISE) -> -EPERM).
                // The container's inheritable set is always empty (capget reports CapInh=0), so no RAISE
                // can ever be satisfied -> -EPERM, exactly as native. LOWER(3) always succeeds (removing an
                // absent cap is a no-op) and IS_SET(1) reports "not set" against the empty ambient set.
                // Previously RAISE returned 0, falsely claiming the ambient capability had been granted.
                G_RET(c) = (a1 == 2) ? (uint64_t)(-EPERM) : 0;
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
        // exactly the 14 bits of HL_CAP_DEFAULT (g_cap_bnd), so e.g. CAP_SYS_ADMIN(21) reads 0.
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
        // PR_SET_PTRACER (0x59616d61, "Yama"): a process may allow a specific helper pid to ptrace it
        // when Linux's Yama LSM is present. hl has no Yama policy to enforce, so accept the request as a no-op.
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
            g_securebits = (int)a1; // we don't enforce securebits, but the value round-trips via PR_GET_SECUREBITS
            G_RET(c) = 0;
            break;
        }
        // PR_GET_SECUREBITS(27): report the current securebits flags (0 in a default container). The kernel
        // ignores arg2..5 here, so no argument validation -- capsh/libcap read this and it must agree with what
        // PR_SET_SECUREBITS stored. Previously fell through to the generic switch -> -EINVAL (a query that
        // always succeeds on real Linux).
        if ((int)a0 == 27) {
            G_RET(c) = (uint64_t)(unsigned)g_securebits;
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
        // PR_TASK_PERF_EVENTS_DISABLE(31)/PR_TASK_PERF_EVENTS_ENABLE(32): toggle this task's perf events.
        // Always succeeds on real Linux (the remaining args are ignored, no capability required); with no perf
        // subsystem to gate there is nothing to enforce, so mirror the kernel's unconditional success rather
        // than the old blanket -EINVAL, which made a benign query look like an unsupported operation.
        if ((int)a0 == 31 || (int)a0 == 32) {
            G_RET(c) = 0;
            break;
        }
        // PR_MCE_KILL(33)/PR_MCE_KILL_GET(34): the per-process machine-check early/late kill policy. GET (34)
        // returns the stored policy (LATE=0 / EARLY=1 / DEFAULT=2) and rejects any nonzero arg2..5. SET (33)
        // takes a sub-op in arg2: PR_MCE_KILL_CLEAR(0) resets to DEFAULT (arg3..5 must be 0);
        // PR_MCE_KILL_SET(1) takes a policy in arg3 (EARLY=1 / LATE=0 / DEFAULT=2, arg4/5 must be 0). Any other
        // shape is -EINVAL (LTP prctl02). The policy is advisory (there is no guest-visible MCE delivery here)
        // but round-tripping it makes the feature probe succeed exactly as on Linux instead of -EINVAL.
        if ((int)a0 == 34) {
            if (a1 || a2 || a3 || a4) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            G_RET(c) = (uint64_t)(unsigned)g_mce_kill;
            break;
        }
        if ((int)a0 == 33) {
            if (a1 == 0 /*PR_MCE_KILL_CLEAR*/) {
                if (a2 || a3 || a4) {
                    G_RET(c) = (uint64_t)(-EINVAL);
                    break;
                }
                g_mce_kill = 2; // reset to DEFAULT
                G_RET(c) = 0;
                break;
            }
            if (a1 == 1 /*PR_MCE_KILL_SET*/) {
                if (a3 || a4 || a2 > 2 /* EARLY(1)/LATE(0)/DEFAULT(2) only */) {
                    G_RET(c) = (uint64_t)(-EINVAL);
                    break;
                }
                g_mce_kill = (int)a2;
                G_RET(c) = 0;
                break;
            }
            G_RET(c) = (uint64_t)(-EINVAL); // unknown MCE sub-op
            break;
        }
        // PR_SET_MM(35): rewrite fields of this task's mm layout (start_brk, brk, arg/env bounds, ...). The
        // kernel gates the WHOLE option on CAP_SYS_RESOURCE and returns -EPERM before validating the sub-op
        // when it is absent. The container's default cap set (HL_CAP_DEFAULT) does NOT include
        // CAP_SYS_RESOURCE, so every PR_SET_MM here is -EPERM -- matching native (both an unprivileged task
        // and a container root without the cap). Previously this fell into the no-op list and returned 0,
        // silently claiming to relayout the mm while doing nothing.
        if ((int)a0 == 35) {
            G_RET(c) = (g_cap_eff & (1ull << CAP_SYS_RESOURCE)) ? (uint64_t)(-EINVAL) : (uint64_t)(-EPERM);
            break;
        }
        // 0 for known no-ops; EINVAL for unknown (kernel does)
        switch ((int)a0) {
        case 15:
        case 53:
        case 55:
        // NAME/SECCOMP/TIMERSLACK/THP/SPECCTRL...
        case 59: G_RET(c) = 0; break;
        // EINVAL -- so feature probes (e.g. magic "AUXV") fail as on Linux
        default: G_RET(c) = (uint64_t)(-22); break;
        }
        break;
    }
    // Standalone C fallback. Product execution selects the Rust-owned process identity before service().
    // getpid (PID ns: init -> 1)
    case 172: G_RET(c) = (uint64_t)container_pid(); break;
    case 173:
        // getppid (init's parent is 0 in the ns). A restored process reports its recorded guest parent
        // (g_self_gppid), since its live host parent differs after the tree was re-forked.
        if (g_self_gppid >= 0)
            G_RET(c) = (uint64_t)g_self_gppid;
        else if (container_pid() == 1)
            G_RET(c) = 0;
        else {
            pid_t parent = getppid();
            G_RET(c) = (uint64_t)((g_init_hostpid && parent == g_init_hostpid) ? 1 : parent);
        }
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
        // Dynamic Linux namespaces are not yet modeled by the embedded ABI.
        // Silently accepting these bits is observably wrong: callers publish
        // namespace-specific state after a successful clone even though the
        // child remains in the original namespace. Return EINVAL, the kernel
        // contract for unsupported clone flags, so capability probes can use
        // their documented fallback.
        const uint64_t namespace_flags = 0x00020000ull | // CLONE_NEWNS
                                         0x02000000ull | // CLONE_NEWCGROUP
                                         0x04000000ull | // CLONE_NEWUTS
                                         0x08000000ull | // CLONE_NEWIPC
                                         0x10000000ull | // CLONE_NEWUSER
                                         0x20000000ull | // CLONE_NEWPID
                                         0x40000000ull;  // CLONE_NEWNET
        if (a0 & namespace_flags) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
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
        int share_fs = (a0 & 0x00000200ull) != 0; // CLONE_FS
        int fs_status = share_fs ? guest_fs_share() : 0;
        if (fs_status != 0) {
            G_RET(c) = (uint64_t)(int64_t)fs_status;
            break;
        }
        // fork/vfork: COW copy; child continues. Flush RAM-backed scratch into the real (shared) fds so
        // parent and child see one coherent file via the inherited description, exactly as POSIX requires
        // (the heap-resident buffers would otherwise COW-diverge while the fd stays shared).
        memf_materialize_all();
        sigexit_init(); // create the shared guest-signal-death relay in the PARENT before forking, so
                        // this child (and its descendants) inherit the same MAP_SHARED page it may die into.
        bound_fork_state bound_fork;
        int bound_status = bound_fork_prepare(&bound_fork);
        if (bound_status != 0) {
            G_RET(c) = (uint64_t)(int64_t)bound_status;
            break;
        }
        int vfork_pipe[2] = {-1, -1};
        int is_vfork = (a0 & 0x4000) != 0;
        if (is_vfork && pipe(vfork_pipe) != 0) {
            bound_fork_complete(&bound_fork, 0, -1);
            G_RET(c) = (uint64_t)(int64_t)(-errno);
            break;
        }
        if (is_vfork) {
            (void)fcntl(vfork_pipe[0], F_SETFD, FD_CLOEXEC);
            (void)fcntl(vfork_pipe[1], F_SETFD, FD_CLOEXEC);
        }
        int runtime_source_tid = cpu_tid(c);
        if (!hl_target_task_event(c, HL_TASK_EVENT_PREPARE_FORK, 0, (uint64_t)runtime_source_tid, 0)) {
            (void)bound_fork_complete(&bound_fork, 0, -1);
            if (is_vfork) {
                close(vfork_pipe[0]);
                close(vfork_pipe[1]);
            }
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        pid_t pid = fork();
        int fork_error = errno;
        if (is_vfork) {
            if (pid == 0) {
                close(vfork_pipe[0]);
                g_vfork_release_fd = vfork_pipe[1];
            } else {
                close(vfork_pipe[1]);
            }
        }
        bound_status = bound_fork_complete(&bound_fork, pid == 0, pid == 0 ? (int)getpid() : (int)pid);
        if (bound_status != 0) {
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
            if (pid == 0) _exit(127);
            if (pid > 0) {
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
            }
            if (is_vfork && vfork_pipe[0] >= 0) close(vfork_pipe[0]);
            G_RET(c) = (uint64_t)(int64_t)bound_status;
            break;
        }
        if (pid < 0) (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
        errno = fork_error;
        if (pid >= 0) {
            uint64_t child_pid = (uint64_t)(pid == 0 ? getpid() : pid);
            uint64_t source_tid = (uint64_t)runtime_source_tid;
            if (!hl_target_task_event(c, HL_TASK_EVENT_FORK_PROCESS, child_pid, source_tid, pid == 0)) {
                if (pid == 0) _exit(127);
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
                G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
                break;
            }
        }
        if (pid == 0) {
            guest_fs_after_fork(share_fs);
            // clone(child_stack): Linux resumes the child on the supplied stack regardless of CLONE_VM.
            // glibc seeds its clone trampoline (fn ptr + arg) there before the syscall. Restricting this to
            // CLONE_VM made an ordinary process clone resume on the parent's stack: the child then popped a
            // return address as its callback and returned into the caller with cleared callee-saved state.
            // a1==0 is the fork-compatible form and keeps the inherited stack.
            if (a1) G_SP(c) = a1;
            fork_child_hooks(c); // shared child-side engine reset (cache re-alias, caches, kqueues, locks)
            // CLONE_CHILD_SETTID(0x01000000): store the child's own tid (== its pid for a process clone) into
            // the child's *ctid (a4). CLONE_CHILD_CLEARTID(0x00200000): remember ctid so thread/process exit
            // zeroes it and FUTEX_WAKEs a joiner. The old code ignored both, so pthread/runtime handshakes
            // that read the child tid from these slots saw stale memory.
            if ((a0 & 0x01000000) && a4) {
                int tid = (int)getpid();
                (void)guest_copy_to(a4, &tid, sizeof tid);
            }
            if (a0 & 0x00200000) c->ctid = a4;
        }
        // CLONE_PIDFD(0x1000): the kernel stores a pidfd for the new child at the address in `parent_tid`
        // (a2, the aarch64 clone slot). Mint a host pollable process watch through pidfd_make; modern runtimes
        // (Go/Rust/glibc posix_spawn) then epoll_wait/poll THAT fd to reap the
        // compiler child they just forked. Without it the guest's pidfd storage keeps its stale value (Go
        // seeds it 0 -> fd 0 = stdin) and the wait blocks forever at 0% CPU -- the go/npm/cargo build hang.
        if (pid > 0 && (a0 & 0x1000) && a2) {
            int pfd = pidfd_make(pid);
            if (pfd >= 0 && guest_copy_to(a2, &pfd, sizeof pfd) != sizeof pfd) {
                close(pfd);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (pid > 0) { // parent side of a successful fork: count it for /proc/stat processes + pids.current
            atomic_fetch_add(&g_forks_since_boot, 1);
            proc_reg_mark_child((int)pid); // guest-pid namespace: register the child NOW (parent-side, race-
                                           // free) so a kill/pidfd membership check can never ESRCH it before
                                           // it runs its own proc_reg_after_fork publish
            acct_child_born((int)pid);     // register the child's OWN task slot (container-wide pids.current)
        }
        if (pid > 0 && is_vfork) {
            unsigned char committed;
            while (read(vfork_pipe[0], &committed, sizeof committed) < 0 && errno == EINTR) {}
            close(vfork_pipe[0]);
        } else if (pid < 0 && is_vfork) {
            close(vfork_pipe[0]);
        }
        // CLONE_PARENT_SETTID(0x00100000): store the child's tid (its pid) into the PARENT's *ptid (a2).
        // Mutually exclusive with CLONE_PIDFD (which also uses the ptid slot), so it never clobbers a pidfd.
        if (pid > 0 && (a0 & 0x00100000) && !(a0 & 0x1000) && a2) {
            int parent_tid = (int)pid;
            if (guest_copy_to(a2, &parent_tid, sizeof parent_tid) != sizeof parent_tid) {
                G_RET(c) = (uint64_t)(int64_t)-EFAULT;
                break;
            }
        }
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
            int have_path = 0;
            if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_fdpath[(int)a0][0]) {
                snprintf(hb, sizeof hb, "%s", g_fdpath[(int)a0]);
                have_path = 1;
            } else if ((int)a0 >= 0 && hl_native_fd_path((int)a0, hb, sizeof hb) == 0 && hb[0]) {
                have_path = 1;
            }
            if (!have_path) {
                G_RET(c) = (uint64_t)(int64_t)(((int)a0 < 0) ? -EBADF : -EACCES);
                break;
            }
            if (g_rootfs) {
                char gb[4200];
                int mapped = guest_from_host_raw(hb, gb, sizeof gb);
                if (mapped <= 0) {
                    G_RET(c) = (uint64_t)(int64_t)(mapped < 0 ? mapped : -EACCES);
                    break;
                }
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
                if ((int)a0 >= 0 && (int)a0 < HL_NFD && g_fdpath[(int)a0][0])
                    snprintf(hb, sizeof hb, "%s", g_fdpath[(int)a0]);
                else if (hl_native_fd_path((int)a0, hb, sizeof hb) != 0) {
                    G_RET(c) = (uint64_t)(int64_t)(-EBADF);
                    break;
                }
                if (path_join(eat_pb, sizeof eat_pb, hb, ep) != 0) {
                    G_RET(c) = (uint64_t)(int64_t)(-ENAMETOOLONG);
                    break;
                }
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
            const char *name = cs ? cs + 1 : (cb ? cb : "");
            size_t name_length = strnlen(name, sizeof comm_src - 1);
            memcpy(comm_src, name, name_length);
            comm_src[name_length] = 0; // Linux path components are NAME_MAX; set_guest_comm applies TASK_COMM_LEN
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
                    if (hl_native_fd_path(pfn, hb, sizeof hb) == 0 && hb[0]) {
                        if (g_rootfs) {
                            char gb[4200];
                            int mapped = guest_from_host_raw(hb, gb, sizeof gb);
                            if (mapped <= 0) {
                                G_RET(c) = (uint64_t)(int64_t)(mapped < 0 ? mapped : -EACCES);
                                break;
                            }
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
        // execve(2) error classification, matching Linux binfmt semantics, applied to the resolved target:
        // a directory is EACCES, and a regular file that is neither an ELF this engine can translate nor a
        // #! script is ENOEXEC. A missing path stat()s ENOENT and falls through to the access() check below.
        // A bare-mode (no-rootfs) launch used to additionally require the target to BE the launched image,
        // collapsing every other target to ENOENT -- `sh -c 'exec bash'` reported "not found" while the same
        // binary launched directly ran. The engine reads the target through the same host services either
        // way, so the gate isolated nothing; an unreadable or unloadable image still fails right here.
        {
            struct stat xst;
            if (stat(p, &xst) == 0) {
                if (S_ISDIR(xst.st_mode)) {
                    G_RET(c) = (uint64_t)(int64_t)(-EACCES);
                    break;
                }
                if (S_ISREG(xst.st_mode)) {
                    FILE *xf = fopen(p, "rb");
                    if (xf) {
                        unsigned char hdr[20] = {0};
                        size_t got = fread(hdr, 1, sizeof hdr, xf);
                        fclose(xf);
                        int is_elf = got >= 4 && hdr[0] == 0x7f && hdr[1] == 'E' && hdr[2] == 'L' && hdr[3] == 'F';
                        int is_script = got >= 2 && hdr[0] == '#' && hdr[1] == '!';
                        if (!is_elf && !is_script) {
                            G_RET(c) = (uint64_t)(int64_t)(-ENOEXEC);
                            break;
                        }
                        // An ELF this engine cannot translate (32-bit, or a foreign e_machine) is ENOEXEC --
                        // Linux reports that for an image no binfmt handler accepts. load_elf() would instead
                        // exit(1) the whole engine, and the exec commit point below is past every return.
                        if (is_elf &&
                            (got < 20 || hdr[4] != 2 || (unsigned)(hdr[18] | (hdr[19] << 8)) != HL_EXEC_ELF_MACHINE)) {
                            G_RET(c) = (uint64_t)(int64_t)(-ENOEXEC);
                            break;
                        }
                    }
                }
            }
        }
        if (access(p, F_OK) != 0) {
            G_RET(c) = (uint64_t)(-2);
            break;
            // ENOENT
        }
        char *argv[HL_MAXARGV];        // Linux allows far more than 255 args within ARG_MAX -- a fixed 256 silently
        int ac = 0;                    // dropped the tail (a different command ran, and /proc/self/cmdline diverged)
        uint64_t *gv = (uint64_t *)a1; // a1 (argv array base) already nonpie_p()'d at the top redirect
        while (gv && gv[ac] && ac < HL_MAXARGV - 1) {
            argv[ac] = (char *)nonpie_p(gv[ac]); // each argv[] element may itself be a low-image pointer
            ac++;
        }
        argv[ac] = NULL;
        // Forward the guest's ACTUAL environment across the exec: build_stack rebuilds the new process env
        // from HL_GUEST_ENV, so serialize envp (a2) into it NOW while guest memory is still mapped. A guest
        // that set/modified env vars (FOO=bar, a tweaked PATH) thus sees them survive; a NULL envp keeps the
        // container's HL_GUEST_ENV defaults (a2 is NOT rebased by the dispatch redirect, unlike a0/a1).
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
        char *na[HL_MAXARGV];
        int nn = 0;
        // Linux passes the execve path (a0) as the script-path arg; the original argv[0] is discarded.
        na[nn++] = (char *)a0;
        for (int i = 1; i < ac && nn < HL_MAXARGV - 1; i++)
            na[nn++] = argv[i];
        na[nn] = NULL;
        const char *sh_finalhost;
        int sh_new = resolve_shebang_chain(na, nn, HL_MAXARGV, p, sh_store, shpb, sizeof shpb, &sh_finalhost);
        if (sh_new < 0) {
            // too many nested #! -> ELOOP. `-ELOOP` is the host macOS errno 62; svc_done's boundary translation
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
        if (!thread_exec_owner_handoff(c)) {
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        thread_exit_others(c);
        // All failure returns are behind us: Linux releases a vfork parent
        // when exec commits, before the new image begins executing.
        vfork_release_parent();
        set_guest_comm(comm_src); // comm := basename of the exec'd NAME (captured pre-rewrite above)
        cred_after_exec(p);       // apply set-id ownership, recompute caps, and clear KEEPCAPS
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
        char *xargv[HL_MAXARGV];
        for (int i = 0; i < ac && i < HL_MAXARGV - 1; i++)
            xargv[i] = strdup(argv[i]);
        xargv[ac < HL_MAXARGV - 1 ? ac : HL_MAXARGV - 1] = NULL;
        bound_mapping_reset();
        hl_gmap_reset();
        gna_reset();                   // the old image's PROT_NONE ranges are gone with its address space
        hl_gmap_lock_reset();          // ... and so are its mlock'd ranges (VmLck resets across execve)
        g_nonpie_lo = g_nonpie_hi = 0; // reset; load_elf re-sets it iff the new main image is non-PIE
        p = xpath;
        for (int i = 0; i < ac && i < HL_MAXARGV - 1; i++)
            argv[i] = xargv[i];
        argv[ac < HL_MAXARGV - 1 ? ac : HL_MAXARGV - 1] = NULL;
        struct loaded lm;
        char pc_ihost[4200];
        const char *pc_interp_host = NULL;
        (void)pc_ihost;
        (void)pc_interp_host;
#ifdef PCACHE_EXEC_HOOKS
        pcache_exec_force_main(); // map the new image at the fixed VA so its cached arena is reusable
#endif
        load_elf(p, &lm, NULL);
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
            load_elf(ih, &li, NULL);
            jump = li.entry;
            at_base = li.base;
        }
        g_cp = g_cache;
        /* Translation-map visibility is generation-tagged. Clearing only the
           record payload leaves the old generation slots logically live and
           lets the new exec image observe zero/stale translation records. */
        map_clear();
        // flush old translations
        pend_reset();
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
        uint64_t heap;
        if (hl_gmap_map_anonymous(0, 256u << 20, HL_HOST_MEMORY_READ | HL_HOST_MEMORY_WRITE, HL_HOST_MEMORY_PRIVATE,
                                  &heap) != HL_STATUS_OK)
            _exit(127);
        brk_lo = brk_cur = heap;
        brk_hi = brk_lo + (256u << 20);
        // Publish the new image's exec path BEFORE build_stack: build_stack points AT_EXECFN at
        // g_exe_path (the canonical guest exec pathname), and /proc/self/exe reads it too.
        snprintf(g_exe_path_store, sizeof g_exe_path_store, "%s", gexe); // /proc/self/exe -> the new image
        g_exe_path = g_exe_path_store;
        uint64_t sp = build_stack(ac, argv, &lm, at_base);
        proc_reg_publish(gexe, ac, argv); // republish the process table entry (comm/argv changed on exec)
        free(xpath);
        for (int i = 0; i < ac && i < HL_MAXARGV - 1; i++) // mirror the strdup loop bound above; a 255 cap
            free(xargv[i]);                                // leaked xargv[255..ac-1] on every argc>255 execve
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
        int status_efault = 0;
        pid_t r;
        struct rusage ruloc;
        memset(&ruloc, 0, sizeof ruloc);
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
        if (a3 && guest_accessible_prefix(a3, 144, HL_LOGICAL_VMA_WRITE) != 144) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // checkpoint restore: a wait targeting a specific checkpoint-time guest pid must name the live host
        // pid the tree was re-forked with (identity no-op on a normal launch when the pid map is empty).
        if (hl_linux_pidmap_count(&g_pidmap) != 0 && (int)a0 > 0)
            a0 = (uint64_t)(unsigned)hl_linux_pidmap_host(&g_pidmap, (int)a0);
        else if (hl_linux_pidmap_count(&g_pidmap) != 0 && (int)a0 < -1)
            a0 = (uint64_t)(int64_t)(-hl_linux_pidmap_host(&g_pidmap, -(int)a0));
        // when ptrace is already in use in this session (a tracee link exists -> nactive>0) route the
        // wait through the ptrace pump, which surfaces tracee ptrace-stops (Linux-encoded) AND real child
        // exits and tears a link down when its tracee dies. For the ENTIRE non-ptrace matrix nactive is 0,
        // so this predicate is false. Returns 1 when it produced a result (r/st Linux-encoded).
        if (ptrace_wait_active()) {
            pid_t pr;
            int handled = ptrace_wait(c, (pid_t)(int)a0, (int)a2, a3 ? &ruloc : NULL, &st, &pr);
            if (handled) {
                if (pr < 0) {
                    G_RET(c) = (uint64_t)(int64_t)pr;
                    break;
                } // -errno / -EINTR
                if (a1 && guest_copy_to(a1, &st, sizeof st) != sizeof st) {
                    G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                    break;
                }
                if (a3) {
                    uint8_t linux_ru[144];
                    rusage_to_linux(linux_ru, &ruloc);
                    if (guest_copy_to(a3, linux_ru, sizeof linux_ru) != sizeof linux_ru) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        break;
                    }
                }
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
                if (ptrace_wait(c, (pid_t)(int)a0, (int)a2, a3 ? &ruloc : NULL, &st, &pr)) {
                    pt_wait_disarm(pt_armed, &pt_saved);
                    ts_wait_leave();
                    if (pr < 0) {
                        G_RET(c) = (uint64_t)(int64_t)pr;
                        goto wait_done;
                    }
                    if (a1 && guest_copy_to(a1, &st, sizeof st) != sizeof st) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        goto wait_done;
                    }
                    if (a3) {
                        uint8_t linux_ru[144];
                        rusage_to_linux(linux_ru, &ruloc);
                        if (guest_copy_to(a3, linux_ru, sizeof linux_ru) != sizeof linux_ru) {
                            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                            goto wait_done;
                        }
                    }
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
#if defined(__APPLE__)
            // A raw macOS SIGBUS from a translated child is the host's bad-address/protection fault;
            // Linux reports it as SIGSEGV. Intentional guest SIGBUS deaths use the shared sigexit relay.
            int lsig = rawsig == SIGBUS ? 11 : sig_m2l(rawsig) & 0x7f;
#else
            int lsig = sig_m2l(rawsig) & 0x7f;
#endif
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
        if (a3 && r > 0) {
            uint8_t linux_ru[144];
            rusage_to_linux(linux_ru, &ruloc);
            if (guest_copy_to(a3, linux_ru, sizeof linux_ru) != sizeof linux_ru) {
                status_efault = 1;
                goto wait_reap_bookkeeping;
            }
        }
        // status copy_to_user: a non-NULL but unwritable status pointer is -EFAULT, exactly as the kernel's
        // put_user(status, stat_addr) after the child is already reaped (native wait4 releases the zombie THEN
        // faults, leaving nothing to re-reap -- verified on aarch64). Guard the direct guest write so a bad
        // pointer returns EFAULT instead of faulting the engine; the reap-side bookkeeping below still runs.
        status_efault = a1 && guest_copy_to(a1, &st, sizeof st) != sizeof st;
    wait_reap_bookkeeping:
        // guest-pid namespace: a reaped child that TERMINATED (exited or signalled -- not merely stopped
        // 0x7f / continued 0xffff) leaves the pid table; drop its container-registry record here so a
        // signal-killed child (which never ran its own exit cleanup) can't leave a stale membership marker
        // that a recycled host pid could inherit. Use the host pid `r` before the restore remap below.
        if (r > 0 && (st & 0xff) != 0x7f && st != 0xffff) proc_reg_reap((int)r);
        // checkpoint restore: report the reaped child under the guest pid the checkpoint recorded, and drop
        // its translation once it is reaped so a future host pid can never alias it (no-op on normal launch).
        if (hl_linux_pidmap_count(&g_pidmap) != 0 && r > 0) {
            int gp = hl_linux_pidmap_guest(&g_pidmap, (int)r);
            if (gp != (int)r) {
                if (((st & 0x7f) == 0) || (((st & 0x7f) != 0x7f) && ((st & 0x7f) != 0)))
                    (void)hl_linux_pidmap_remove_host(&g_pidmap, (int)r);
                r = (pid_t)gp;
            }
        }
        int runtime_reap_fault =
            r > 0 && (st & 0xff) != 0x7f && st != 0xffff &&
            !hl_target_task_event(c, HL_TASK_EVENT_REAP_PROCESS, (uint64_t)r,
                                  (uint64_t)(c->tid ? c->tid : container_pid()), (uint64_t)(uint32_t)st);
        G_RET(c) = status_efault        ? (uint64_t)(int64_t)(-EFAULT)
                   : runtime_reap_fault ? (uint64_t)(int64_t)(-EIO)
                                        : (uint64_t)r;
    wait_done:; // the EINTR reroute jumps here (G_RET + *status already set)
        break;
    }
    case 261: {
        // prlimit64(pid, resource, NEW, OLD): report the CURRENT limit into OLD first (so a combined
        // get+set returns the pre-change value), THEN apply NEW into the per-resource store so a later
        // get reflects it. glibc's getrlimit/setrlimit/prlimit all funnel through this syscall, so the
        // store (g_limits, also seeded by docker --ulimit) is the single source of truth. without
        // applying NEW, setrlimit "succeeded" but the value never took -- the next getrlimit saw the old.
        int res = (int)a1;
        // Linux validates BEFORE touching the limits: the task lookup runs first (a negative or dead target
        // pid -> ESRCH), then the resource number is range-checked (>= RLIM_NLIMITS(16) -> EINVAL). Without
        // these hl reports success for dead pids and unsupported resources, so probes see them as valid.
        if ((int)a0 < 0 || sched_pid_live((int)a0) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        if (res < 0 || res >= HL_LIMIT_COUNT) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        uint64_t new_limit[2], old_limit[2];
        if ((a2 && guest_copy_from(new_limit, a2, sizeof(new_limit)) != sizeof(new_limit)) ||
            (a3 && guest_accessible_prefix(a3, sizeof(old_limit), PROT_WRITE) != sizeof(old_limit))) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a3) {
            svc_fill_rlimit(res, old_limit);
            if (guest_copy_to(a3, old_limit, sizeof(old_limit)) != sizeof(old_limit)) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (a2) {
            const uint64_t *nl = new_limit;
            uint64_t ncur = nl[0], nmax = nl[1];
            // Linux: soft may not exceed hard -> EINVAL (RLIM_INFINITY == ~0 is the max, so it never trips).
            if (ncur != ~0ull && nmax != ~0ull && ncur > nmax) {
                G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
                break;
            }
            if (res == 7) {
                uint32_t guest_limit = hl_engine_guest_fd_limit();
                uint64_t ceiling = guest_limit > 0 ? guest_limit : 20480;
                if (nmax == ~0ull || nmax > ceiling || ncur == ~0ull || ncur > ceiling) {
                    G_RET(c) = (uint64_t)(int64_t)(-EPERM);
                    break;
                }
            }
            hl_limit_table_set(&g_limits, res, ncur, nmax);
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
        uint64_t ca[8];
        if (guest_copy_from(ca, a0, sizeof ca) != sizeof ca) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint64_t flags = ca[0];
        const uint64_t namespace_flags = 0x00020000ull | 0x02000000ull | 0x04000000ull | 0x08000000ull | 0x10000000ull |
                                         0x20000000ull | 0x40000000ull;
        if (flags & namespace_flags) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
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
        int share_fs = (flags & 0x00000200ull) != 0; // CLONE_FS
        int fs_status = share_fs ? guest_fs_share() : 0;
        if (fs_status != 0) {
            G_RET(c) = (uint64_t)(int64_t)fs_status;
            break;
        }
        sigexit_init(); // shared signal-death relay must exist in the parent before fork (see case 220)
        bound_fork_state bound_fork;
        int bound_status = bound_fork_prepare(&bound_fork);
        if (bound_status != 0) {
            G_RET(c) = (uint64_t)(int64_t)bound_status;
            break;
        }
        int runtime_source_tid = cpu_tid(c);
        if (!hl_target_task_event(c, HL_TASK_EVENT_PREPARE_FORK, 0, (uint64_t)runtime_source_tid, 0)) {
            (void)bound_fork_complete(&bound_fork, 0, -1);
            G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
            break;
        }
        pid_t pid = fork();
        int fork_error = errno;
        bound_status = bound_fork_complete(&bound_fork, pid == 0, pid == 0 ? (int)getpid() : (int)pid);
        if (bound_status != 0) {
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
            if (pid == 0) _exit(127);
            if (pid > 0) {
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
            }
            G_RET(c) = (uint64_t)(int64_t)bound_status;
            break;
        }
        if (pid < 0)
            (void)hl_target_task_event(c, HL_TASK_EVENT_CANCEL_FORK, 0, (uint64_t)runtime_source_tid, 0);
        else {
            uint64_t child_pid = (uint64_t)(pid == 0 ? getpid() : pid);
            if (!hl_target_task_event(c, HL_TASK_EVENT_FORK_PROCESS, child_pid, (uint64_t)runtime_source_tid,
                                      pid == 0)) {
                if (pid == 0) _exit(127);
                int failed_status;
                kill(pid, SIGKILL);
                while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
                G_RET(c) = (uint64_t)(int64_t)-EAGAIN;
                break;
            }
        }
        errno = fork_error;
        // child: the same shared engine reset as the clone/fork site above (cache re-alias / §B shadow /
        // path caches / kqueues / fork-unsafe locks). clone3 historically lacked the W^X re-assert and the
        // DIR*-cache drop the clone site had; the shared helper closes that drift.
        if (pid == 0) {
            guest_fs_after_fork(share_fs);
            if ((flags & 0x100) && ca[5]) G_SP(c) = ca[5] + ca[6];
            fork_child_hooks(c);
            // clone_args: child_tid = ca[2]. CLONE_CHILD_SETTID stores the child's tid there; CLONE_CHILD_
            // CLEARTID remembers it so exit zeroes it + wakes a joiner (mirrors case 220).
            if ((flags & 0x01000000) && ca[2]) {
                int tid = (int)getpid();
                (void)guest_copy_to(ca[2], &tid, sizeof tid);
            }
            if (flags & 0x00200000) c->ctid = ca[2];
        }
        // CLONE_PIDFD: clone3 stores the child pidfd via the `pidfd` field (clone_args[1]); back it the same
        // way as case 220 so a clone3-based spawn (newer glibc/runtimes) can epoll_wait/poll it to reap.
        if (pid > 0 && (flags & 0x1000) && ca[1]) {
            int pfd = pidfd_make(pid);
            if (pfd >= 0 && guest_copy_to(ca[1], &pfd, sizeof pfd) != sizeof pfd) {
                close(pfd);
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (pid > 0) { // parent side of a successful clone3 fork: count it (see case 220)
            atomic_fetch_add(&g_forks_since_boot, 1);
            proc_reg_mark_child((int)pid); // guest-pid namespace: parent-side registration (see case 220)
            acct_child_born((int)pid);     // register the child's OWN task slot (container-wide pids.current)
        }
        // clone_args: parent_tid = ca[3]. CLONE_PARENT_SETTID stores the child's tid (pid) into the PARENT's
        // parent_tid (a distinct field from pidfd in clone3, so it never conflicts with CLONE_PIDFD).
        if (pid > 0 && (flags & 0x00100000) && ca[3]) {
            int tid = (int)pid;
            if (guest_copy_to(ca[3], &tid, sizeof tid) != sizeof tid) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        G_RET(c) = pid < 0 ? (uint64_t)(-errno) : (uint64_t)pid;
        break;
    }
    default: return 0;
    }
    return svc_done(c); // boundary errno xlate (host macOS -> Linux); see helpers.c svc_done
}
