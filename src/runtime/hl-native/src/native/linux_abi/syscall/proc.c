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

enum { HL_EXEC_ARGUMENT_BYTES = 128 * 1024 };

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
    // The dispatcher already rebases the envp array base for a displaced
    // ET_EXEC image. Rebase only its pointer elements here; applying the
    // projection twice loses an x86 exec's environment.
    uint64_t *ev = (uint64_t *)envp_guest;
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

#include "../../engine/result.h"

// CLONE_VFORK suspends the calling parent thread until the child either
// commits an exec or exits. The engine implements process clones with host
// fork(), so use a private one-byte rendezvous to preserve that ordering.
// Without it, glibc may free the temporary child stack as soon as clone
// returns in the parent while the child is still using that stack to enter
// execve, producing an intermittent pre-exec SIGILL under compiler load.
static int g_vfork_release_fd = -1;
static int g_vfork_ack_fd = -1;

static void vfork_release_parent(void) {
    if (g_vfork_release_fd < 0) return;
    unsigned char committed = 1;
    while (write(g_vfork_release_fd, &committed, sizeof committed) < 0 && errno == EINTR) {}
    close(g_vfork_release_fd);
    g_vfork_release_fd = -1;
}

static void vfork_publish_exit(void) {
    if (g_vfork_release_fd < 0) return;
    unsigned char committed = 2;
    while (write(g_vfork_release_fd, &committed, sizeof committed) < 0 && errno == EINTR) {}
    if (g_vfork_ack_fd >= 0) {
        while (read(g_vfork_ack_fd, &committed, sizeof committed) < 0 && errno == EINTR) {}
        close(g_vfork_ack_fd);
        g_vfork_ack_fd = -1;
    }
    close(g_vfork_release_fd);
    g_vfork_release_fd = -1;
}

static void vfork_import_guest_memory(pid_t child) {
#if defined(__linux__)
    for (size_t index = 0; index < hl_gmap_count(); index++) {
        hl_gmap_entry entry;
        if (!hl_gmap_get(index, &entry) || entry.physical_length == 0) continue;
        struct iovec local = {(void *)(uintptr_t)entry.physical_address, (size_t)entry.physical_length};
        struct iovec remote = local;
        (void)process_vm_readv(child, &local, 1, &remote, 1, 0);
    }
#else
    (void)child;
#endif
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
    if (hl_vfs_cursor_state_after_fork() != 0) {
        c->exit_code = 70;
        c->exited = 1;
        return;
    }
    g_ndirs = 0;                 // the getdents DIR* cache is the PARENT's -- closedir'ing inherited handles
                                 // (on the child's close) crashes; drop it so the child re-fdopendir's fresh
    kqueue_rebuild_after_fork(); // macOS kqueue() fds (epoll/timerfd/inotify) don't survive fork ->
                                 // rebuild them so the child doesn't EBADF on its inherited event fds
                                 // (also reinits g_ep_mtx, inherited-locked if a peer forked mid-epoll)
    thread_after_fork();         // reset process-private thread/futex locks a dead peer may have held at fork
    signal_after_fork(c);        // Linux does not inherit either process- or thread-pending signals
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

#include "process/identity.c"
#include "process/futex.c"
#include "process/scheduling.c"
#include "process/control.c"
#include "process/exec.c"
#include "process/clone.c"
#include "process/wait.c"

#define HL_PROC_CASE(number)                                                                                          \
    case number:                                                                                                      \
        (void)svc_proc_##number(c, nr, a0, a1, a2, a3, a4, a5);                                                      \
        break

static int svc_proc(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                    uint64_t a5) {
    switch (nr) {
        HL_PROC_CASE(51);
        HL_PROC_CASE(90);
        HL_PROC_CASE(91);
        HL_PROC_CASE(92);
        HL_PROC_CASE(93);
        HL_PROC_CASE(94);
        HL_PROC_CASE(96);
        HL_PROC_CASE(97);
        HL_PROC_CASE(98);
        HL_PROC_CASE(99);
        HL_PROC_CASE(116);
        HL_PROC_CASE(118);
        HL_PROC_CASE(119);
        HL_PROC_CASE(120);
        HL_PROC_CASE(121);
        HL_PROC_CASE(122);
        HL_PROC_CASE(123);
        HL_PROC_CASE(124);
        HL_PROC_CASE(125);
        HL_PROC_CASE(126);
        HL_PROC_CASE(127);
        HL_PROC_CASE(140);
        HL_PROC_CASE(141);
        HL_PROC_CASE(143);
        HL_PROC_CASE(144);
        HL_PROC_CASE(145);
        HL_PROC_CASE(146);
        HL_PROC_CASE(147);
        HL_PROC_CASE(148);
        HL_PROC_CASE(149);
        HL_PROC_CASE(150);
        HL_PROC_CASE(151);
        HL_PROC_CASE(152);
        HL_PROC_CASE(154);
        HL_PROC_CASE(155);
        HL_PROC_CASE(156);
        HL_PROC_CASE(158);
        HL_PROC_CASE(159);
        HL_PROC_CASE(165);
        HL_PROC_CASE(167);
        HL_PROC_CASE(172);
        HL_PROC_CASE(173);
        HL_PROC_CASE(174);
        HL_PROC_CASE(175);
        HL_PROC_CASE(176);
        HL_PROC_CASE(177);
        HL_PROC_CASE(178);
        HL_PROC_CASE(220);
        HL_PROC_CASE(221);
        HL_PROC_CASE(260);
        HL_PROC_CASE(261);
        HL_PROC_CASE(268);
        HL_PROC_CASE(281);
        HL_PROC_CASE(435);
    default: return 0;
    }
    return svc_done(c);
}

#undef HL_PROC_CASE
