struct hl_linux_bpf_filter;
static int checkpoint_relay_after_fork(void);
static int checkpoint_relay_start(void);

static void thread_after_fork(void) {
    /* A vanished parent thread may have held a decoder publication lease or
       registered writer intent. The private child cannot discharge either, so
       conservatively disable byte-authorized memo hits in its inherited view. */
    hl_guest_fetch_authority_after_fork_child();
    pthread_mutex_init(&g_threg_m, NULL); // thread registry (tkill/tgkill lookup, thread_register)
    struct cpu *self = (g_my_threg >= 0) ? g_threg[g_my_threg].c : NULL;
    pthread_mutex_init(&g_process_owner_lock, NULL);
    pthread_cond_init(&g_process_owner_cond, NULL);
    g_process_owner_cpu = self;
    g_process_exec_cpu = NULL;
    g_process_exec_pending = 0;
    g_process_exec_complete = 0;
    // The sole thread surviving fork becomes the child process leader. Linux
    // requires its tid to equal the new pid even when a non-leader parent
    // thread called fork. Keeping the caller's old tid gives unrelated child
    // processes duplicate thread identities and breaks process-shared users
    // that key ownership by tid.
    if (self) self->tid = 0;
    /*
     * Only the calling host thread survives a guest fork. A vanished peer may
     * have held either process-private file-map lock while publishing or
     * replaying shared mutations. The child owns a private copy of the
     * registry and cursor, so rebuild both locks before its first replay.
     */
    pthread_mutex_init(&g_filemap_lock, NULL);
    pthread_mutex_init(&g_filemap_replay_lock, NULL);
    // SINGLE-THREADED-PARENT FAST PATH: with no peer thread at fork, no private-futex bucket lock could have
    // been held (bucket locks are taken only transiently by the holder, never across a guest fork) and no
    // waiter could be parked, and the tid registry already lists only the calling thread. The entire reset
    // below (256 bucket re-inits + a 128KB registry memset, ~130us) exists solely to repair peer-induced
    // damage, so a lone forker can skip all of it -- the inherited state is already exactly correct. The
    // g_shkey_m lock re-init is likewise unnecessary (no peer to hold it), but it is O(1) so we keep it
    // unconditionally rather than reason about it per branch.
    pthread_mutex_init(&g_shkey_m, NULL);
    if (g_threg_live <= 1) {
        g_fbk_active = g_fbk_private; // the child's __thread active pointer must still select the private table
        if (checkpoint_relay_after_fork() != 0 && self) {
            self->exit_code = 70;
            self->exited = 1;
        }
        hl_guest_fetch_authority_after_fork_rebind();
        hl_x86_decode_after_fork_rebind();
        return;
    }
    futex_private_table_after_fork();
    // fork() clones ONLY the calling thread, but the tid->thread registry still lists every PARENT thread.
    // Those phantom entries never unregister (no thread backs them in the child), so they (1) poison
    // tgkill/tkill routing -- thread_target_signal matches a phantom by tid (or pid 1 for the main thread)
    // and "delivers" the signal onto a dead cpu, dropping a real thread's SIGURG/preempt (Go's async
    // preemption then spins one goroutine at 100% while its peers park -- the livelock) -- and (2) make
    // the child's next execve teardown (thread_exit_others) busy-wait its full ~10s ceiling for phantom peers
    // that can never leave (the go build fork+exec stall: ~14s PER compile child, measured). Rebuild the
    // registry to hold ONLY the surviving (calling) thread, exactly as stw_after_fork() does for the STW
    // registry -- reinitialised this module's LOCKS but left the registry CONTENTS inherited.
    memset(g_threg, 0, sizeof g_threg);
    if (self) {
        g_threg[0].c = self;
        g_threg[0].th = pthread_self();
        g_my_threg = 0;
        g_threg_live = 1; // only the calling thread survives the fork
    } else {
        g_my_threg = -1;
        g_threg_live = 0;
    }
    if (checkpoint_relay_after_fork() != 0 && self) {
        self->exit_code = 70;
        self->exited = 1;
    }
    hl_guest_fetch_authority_after_fork_rebind();
    hl_x86_decode_after_fork_rebind();
}

// A dedicated host signal used only to INTERRUPT a sibling guest thread out of a blocking host syscall
// (kevent/read/poll/nanosleep/...) so it observes cpu->exited and leaves the dispatcher -- see
// thread_exit_others (execve teardown). macOS has no realtime signals; SIGINFO(29) is unused by the guest
// signal map (sig_l2m omits 7/EMT and 29/INFO), so a process-wide handler for it cannot collide with an
// emulated guest signal -- the same free-signal reasoning the STW code uses for SIGEMT.
#define THREAD_INT_SIG SIGINFO

static _Thread_local struct cpu *g_thread_cpu;
static _Atomic int g_ckpt_relay_writer = -1;
static _Atomic uint32_t g_ckpt_relay_requested;
static _Atomic uint32_t g_ckpt_relay_queued;
static _Atomic uint32_t g_ckpt_relay_handlers;
_Static_assert(ATOMIC_INT_LOCK_FREE == 2, "checkpoint signal relay requires lock-free integer atomics");
static int g_ckpt_relay_reader = -1;
static pthread_t g_ckpt_relay_thread;
static int g_ckpt_relay_started;

static int thread_int_is_external(const siginfo_t *information) {
#if defined(SI_TKILL)
    return information == NULL || information->si_code != SI_TKILL;
#elif defined(__APPLE__)
    // Darwin does not expose Linux's SI_TKILL. pthread_kill originates in this
    // process, while the checkpoint coordinator is a different host process.
    return information == NULL || information->si_pid != getpid();
#else
    // Hosts without reliable sender provenance conservatively coalesce the
    // signal. Such hosts do not currently enable checkpoint/restore.
    return 1;
#endif
}

static void thread_int_handler(int sig, siginfo_t *information, void *context) {
    (void)sig;
    (void)context;
    int saved = errno;
    (void)atomic_fetch_add_explicit(&g_ckpt_relay_handlers, 1, memory_order_acquire);
    // Its base job is to make a blocked host syscall return EINTR (empty body suffices). When checkpoint/
    // restore is armed, ALSO set cpu->irq so a chained in-cache guest loop (which never returns to the
    // dispatcher on its own) is bounced out to the safepoint where ckpt_poll runs. Inert on a normal launch
    // (the snapshot state is disabled), so the gate is unchanged; SIGINFO is guest-clobber-proof (sig_l2m omits 29).
    if (hl_linux_snapshot_enabled(&g_ckpt_snapshot)) {
        struct cpu *c = g_thread_cpu;
        if (c) __atomic_store_n(&c->irq, 1, __ATOMIC_RELAXED);
    }
    if (thread_int_is_external(information)) {
        (void)atomic_fetch_add_explicit(&g_ckpt_relay_requested, 1, memory_order_relaxed);
        uint32_t idle = 0;
        if (atomic_compare_exchange_strong_explicit(&g_ckpt_relay_queued, &idle, 1, memory_order_acq_rel,
                                                    memory_order_relaxed)) {
            int writer = atomic_load_explicit(&g_ckpt_relay_writer, memory_order_acquire);
            unsigned char byte = 1;
            if (writer < 0 || (write(writer, &byte, 1) < 0 && errno != EAGAIN))
                atomic_store_explicit(&g_ckpt_relay_queued, 0, memory_order_release);
        }
    }
    (void)atomic_fetch_sub_explicit(&g_ckpt_relay_handlers, 1, memory_order_release);
    errno = saved;
}

static void checkpoint_relay_kick_threads(void) {
    pthread_mutex_lock(&g_threg_m);
    for (int slot = 0; slot < THREAD_REG_MAX; ++slot) {
        struct cpu *cpu = g_threg[slot].c;
        if (cpu == NULL) continue;
        __atomic_store_n(&cpu->irq, 1, __ATOMIC_SEQ_CST);
        pthread_cond_t *condition = __atomic_load_n(&g_threg[slot].waitc, __ATOMIC_SEQ_CST);
        if (condition != NULL) {
            pthread_mutex_t *mutex = g_threg[slot].waitm;
            pthread_mutex_lock(mutex);
            pthread_cond_broadcast(condition);
            pthread_mutex_unlock(mutex);
        }
        if (!pthread_equal(g_threg[slot].th, pthread_self())) (void)pthread_kill(g_threg[slot].th, THREAD_INT_SIG);
    }
    pthread_mutex_unlock(&g_threg_m);
}

static void *checkpoint_relay_main(void *argument) {
    int reader = *(int *)argument;
    sigset_t blocked;
    sigemptyset(&blocked);
    sigaddset(&blocked, THREAD_INT_SIG);
    (void)pthread_sigmask(SIG_BLOCK, &blocked, NULL);
    for (;;) {
        unsigned char byte;
        ssize_t count;
        do
            count = read(reader, &byte, sizeof byte);
        while (count < 0 && errno == EINTR);
        if (count <= 0) break;
        for (;;) {
            uint32_t observed = atomic_load_explicit(&g_ckpt_relay_requested, memory_order_acquire);
            checkpoint_relay_kick_threads();
            atomic_store_explicit(&g_ckpt_relay_queued, 0, memory_order_release);
            if (atomic_load_explicit(&g_ckpt_relay_requested, memory_order_acquire) == observed) break;
            uint32_t idle = 0;
            if (!atomic_compare_exchange_strong_explicit(&g_ckpt_relay_queued, &idle, 1, memory_order_acq_rel,
                                                         memory_order_relaxed))
                break;
        }
    }
    return NULL;
}

static void checkpoint_relay_close_descriptor(int *descriptor) {
    if (*descriptor < 0) return;
    hl_host_process_fd_private_remove(*descriptor);
    (void)close(*descriptor);
    *descriptor = -1;
}

static int checkpoint_relay_unpublish_writer(void) {
    int writer = atomic_exchange_explicit(&g_ckpt_relay_writer, -1, memory_order_acq_rel);
    while (atomic_load_explicit(&g_ckpt_relay_handlers, memory_order_acquire) != 0)
        sched_yield();
    return writer;
}

static void checkpoint_relay_stop(void) {
    sigset_t blocked, previous;
    sigemptyset(&blocked);
    sigaddset(&blocked, THREAD_INT_SIG);
    (void)pthread_sigmask(SIG_BLOCK, &blocked, &previous);
    int writer = checkpoint_relay_unpublish_writer();
    if (writer >= 0) {
        hl_host_process_fd_private_remove(writer);
        (void)close(writer);
    }
    if (g_ckpt_relay_started && !pthread_equal(g_ckpt_relay_thread, pthread_self()))
        (void)pthread_join(g_ckpt_relay_thread, NULL);
    checkpoint_relay_close_descriptor(&g_ckpt_relay_reader);
    g_ckpt_relay_started = 0;
    atomic_store_explicit(&g_ckpt_relay_queued, 0, memory_order_release);
    (void)pthread_sigmask(SIG_SETMASK, &previous, NULL);
}

static void checkpoint_relay_at_exit(void) {
    checkpoint_relay_stop();
}

static int checkpoint_relay_start(void) {
    if (g_ckpt_relay_started) return 0;
    sigset_t blocked, previous;
    sigemptyset(&blocked);
    sigaddset(&blocked, THREAD_INT_SIG);
    (void)pthread_sigmask(SIG_BLOCK, &blocked, &previous);
    int descriptors[2];
    if (pipe(descriptors) != 0) {
        (void)pthread_sigmask(SIG_SETMASK, &previous, NULL);
        return -1;
    }
    int reader = hl_host_process_fd_private_adopt(descriptors[0]);
    int writer = reader >= 0 ? hl_host_process_fd_private_adopt(descriptors[1]) : -1;
    if (reader < 0 || writer < 0) {
        if (reader >= 0) {
            hl_host_process_fd_private_remove(reader);
            (void)close(reader);
        } else {
            (void)close(descriptors[0]);
        }
        if (writer >= 0) {
            hl_host_process_fd_private_remove(writer);
            (void)close(writer);
        } else {
            (void)close(descriptors[1]);
        }
        (void)pthread_sigmask(SIG_SETMASK, &previous, NULL);
        return -1;
    }
    int flags = fcntl(writer, F_GETFL);
    if (flags < 0 || fcntl(writer, F_SETFL, flags | O_NONBLOCK) != 0 || fcntl(reader, F_SETFD, FD_CLOEXEC) != 0 ||
        fcntl(writer, F_SETFD, FD_CLOEXEC) != 0) {
        checkpoint_relay_close_descriptor(&reader);
        checkpoint_relay_close_descriptor(&writer);
        (void)pthread_sigmask(SIG_SETMASK, &previous, NULL);
        return -1;
    }
    g_ckpt_relay_reader = reader;
    atomic_store_explicit(&g_ckpt_relay_requested, 0, memory_order_relaxed);
    atomic_store_explicit(&g_ckpt_relay_queued, 0, memory_order_relaxed);
    if (pthread_create(&g_ckpt_relay_thread, NULL, checkpoint_relay_main, &g_ckpt_relay_reader) != 0) {
        checkpoint_relay_close_descriptor(&g_ckpt_relay_reader);
        checkpoint_relay_close_descriptor(&writer);
        (void)pthread_sigmask(SIG_SETMASK, &previous, NULL);
        return -1;
    }
    atomic_store_explicit(&g_ckpt_relay_writer, writer, memory_order_release);
    g_ckpt_relay_started = 1;
    static int exit_registered;
    if (!exit_registered) {
        exit_registered = 1;
        (void)atexit(checkpoint_relay_at_exit);
    }
    (void)pthread_sigmask(SIG_SETMASK, &previous, NULL);
    return 0;
}

static int checkpoint_relay_after_fork(void) {
    // Only the calling thread survives fork, so the inherited relay cannot be
    // joined. Invalidate both inherited descriptors before publishing a fresh
    // process-private channel.
    sigset_t blocked, previous;
    sigemptyset(&blocked);
    sigaddset(&blocked, THREAD_INT_SIG);
    (void)pthread_sigmask(SIG_BLOCK, &blocked, &previous);
    int writer = atomic_exchange_explicit(&g_ckpt_relay_writer, -1, memory_order_acq_rel);
    // Only the calling thread survives. A parent-side handler count can name a
    // vanished thread and must never be waited on in the child.
    atomic_store_explicit(&g_ckpt_relay_handlers, 0, memory_order_release);
    if (writer >= 0) {
        hl_host_process_fd_private_remove(writer);
        (void)close(writer);
    }
    checkpoint_relay_close_descriptor(&g_ckpt_relay_reader);
    g_ckpt_relay_started = 0;
    atomic_store_explicit(&g_ckpt_relay_queued, 0, memory_order_release);
    int result = g_ckpt_trigger != NULL ? checkpoint_relay_start() : 0;
    (void)pthread_sigmask(SIG_SETMASK, &previous, NULL);
    return result;
}

static pthread_once_t g_thread_int_once = PTHREAD_ONCE_INIT;

static void thread_int_install(void) {
    hl_guest_fetch_set_direct_validator(guest_exec_direct_valid);
    hl_guest_fetch_set_direct_generation(&g_gnx_generation);
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = thread_int_handler;
    sigemptyset(&sa.sa_mask);
    // Host AArch64 executes with the guest SP live.  Keep this engine-only
    // interrupt frame off that stack; NO SA_RESTART still makes a blocking
    // syscall return EINTR so its retry loop can bail on exited.
    sa.sa_flags = SA_ONSTACK | SA_SIGINFO;
    sigaction(THREAD_INT_SIG, &sa, NULL);
}

static void thread_int_ensure_installed(void) {
    pthread_once(&g_thread_int_once, thread_int_install);
}

// The guest tid this cpu answers gettid() with (see proc.c case 178): its own id, or the init's pid 1.
static int cpu_tid(const struct cpu *c) {
    return c->tid ? c->tid : container_pid();
}

/* Defined in signal.c, which follows thread.c in each target unity build. */
static int sfd_any_ready_for_cpu(struct cpu *cpu);

// Publish/clear the wait primitive this thread is blocked on (see the futex_op waits + thread_target_signal).
static void thread_wait_publish(pthread_mutex_t *m, pthread_cond_t *cnd) {
    if (g_my_threg < 0) return;
    g_threg[g_my_threg].waitm = m; // ordered ahead of the waitc store below (a reader only reads it when set)
    __atomic_store_n(&g_threg[g_my_threg].waitc, cnd, __ATOMIC_SEQ_CST);
}

static void thread_wait_clear(void) {
    if (g_my_threg < 0) return;
    __atomic_store_n(&g_threg[g_my_threg].waitc, NULL, __ATOMIC_SEQ_CST);
}

static void thread_register(struct cpu *c) {
    g_thread_cpu = c;
    c->bus_filter = (uint64_t)(uintptr_t)g_bus_page_filter;
    c->bus_force = (uint64_t)(uintptr_t)&g_bus_filter_force;
    thread_int_ensure_installed();
    // Keep THREAD_INT_SIG deliverable on this thread so a peer's execve teardown can interrupt its syscalls.
    sigset_t unb;
    sigemptyset(&unb);
    sigaddset(&unb, THREAD_INT_SIG);
    pthread_sigmask(SIG_UNBLOCK, &unb, NULL);
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (!g_threg[i].c) {
            g_threg[i].th = pthread_self();
            __atomic_store_n(&g_threg[i].waitc, NULL, __ATOMIC_SEQ_CST);
            __atomic_store_n(&g_threg[i].c, c, __ATOMIC_RELEASE);
            g_my_threg = i;
            g_threg_live++;
            break;
        }
    pthread_mutex_unlock(&g_threg_m);
}

/* Atomically attach one filter to every live task in the caller's thread
   group. Linux reports the first incompatible tid instead of partially
   synchronizing the group. */
static long seccomp_tsync_attach(struct cpu *caller, struct hl_linux_bpf_filter *node) {
    long status = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++) {
        struct cpu *peer = g_threg[i].c;
        if (peer && (peer->seccomp_mode != caller->seccomp_mode || peer->seccomp_filters != caller->seccomp_filters)) {
            status = cpu_tid(peer);
            break;
        }
    }
    if (status == 0) {
        for (int i = 0; i < THREAD_REG_MAX; i++) {
            struct cpu *peer = g_threg[i].c;
            if (peer) {
                peer->seccomp_filters = node;
                peer->seccomp_mode = 2;
            }
        }
    }
    pthread_mutex_unlock(&g_threg_m);
    return status;
}

static void thread_unregister(struct cpu *c) {
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c == c) {
            sigq_drop_target_tid(cpu_tid(c));
            __atomic_store_n(&g_threg[i].waitc, NULL, __ATOMIC_SEQ_CST);
            __atomic_store_n(&g_threg[i].c, NULL, __ATOMIC_RELEASE);
            if (g_threg_live > 0) g_threg_live--;
            break;
        }
    pthread_mutex_unlock(&g_threg_m);
    g_my_threg = -1;
    g_thread_cpu = NULL;
}

// Deliver signal `sig` to the guest thread `tid`: set that thread's per-thread pending bit so it (and not
// some other thread) runs the handler at its next dispatcher safepoint. A thread that is preempted while
// running translated code (e.g. Go's sysmon tgkill'ing a worker with SIGURG to stop-the-world) crosses a
// dispatcher boundary continuously, so the per-thread pending is observed promptly without poking the host
// thread. But a thread PARKED in an interruptible futex wait (pthread_cond_wait) reaches no boundary on its
// own, so if the signal is deliverable now we wake it out of that wait (matching a real futex interrupted by
// a signal) -- without this, Go's doAllThreadsSyscall (setuid/setgid across all Ms, via signal 33) hangs
// because a parked sibling M never runs the per-thread-syscall handler the coordinator busy-waits on.
// Returns 1 if the target was found and flagged, 0 if no live thread carries that tid, and -1 when the
// realtime queue is full (the syscall caller reports EAGAIN rather than silently discarding the signal).
static int thread_target_signal_info(int tid, int sig, int tag, int error, int code, uint64_t value, int pid, int uid,
                                     uint64_t address) {
    int found = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c && cpu_tid(g_threg[i].c) == tid) {
            int published =
                thread_directed_signal_publish(g_threg[i].c, sig, tag, error, code, value, pid, uid, address);
            if (published < 0) {
                found = -1;
                break;
            }
            // also kick the target out of any no-syscall in-cache loop so its emitted body check
            // (cpu->irq) exits to the dispatcher and maybe_deliver_signal runs the handler at a boundary.
            __atomic_store_n(&g_threg[i].c->irq, 1, __ATOMIC_SEQ_CST);
            // Load waitc AFTER storing tpending: the seq_cst StoreLoad here pairs with the target's
            // publish-then-recheck in futex_op so the wakeup is never lost. Only wake for a signal that is
            // actionable now (unblocked) -- a blocked one stays pending without interrupting the wait.
            if (cpu_has_actionable_tsig(g_threg[i].c) || sfd_any_ready_for_cpu(g_threg[i].c)) {
                pthread_cond_t *cnd = __atomic_load_n(&g_threg[i].waitc, __ATOMIC_SEQ_CST);
                if (cnd) {
                    pthread_mutex_t *m = g_threg[i].waitm;
                    pthread_mutex_lock(m); // serialize with the target's pre-wait window; broadcast can't be lost
                    pthread_cond_broadcast(cnd);
                    pthread_mutex_unlock(m);
                } else if (!pthread_equal(g_threg[i].th, pthread_self()) &&
                           __atomic_load_n(&g_threg[i].c->in_service, __ATOMIC_SEQ_CST)) {
                    // No published futex wait: the target is either running translated code (cpu->irq already
                    // bounces it to a dispatcher boundary) or PARKED IN A BLOCKING HOST SYSCALL (read/accept/
                    // recv/poll/nanosleep/...), which reaches no boundary on its own. Poke it with
                    // THREAD_INT_SIG (no SA_RESTART) so that syscall returns EINTR; syscall_should_restart
                    // (now tpending-aware) then declines to restart and the guest sees EINTR + the delivered
                    // signal. Harmless (near-empty handler) if the target was in-cache. Skip a self-signal.
                    pthread_kill(g_threg[i].th, THREAD_INT_SIG);
                }
            }
            found = 1;
            break;
        }
    pthread_mutex_unlock(&g_threg_m);
    return found;
}

// Does the live guest thread `tid` currently BLOCK signal `sig`? A thread-directed tkill/tgkill of a signal
// the target has blocked must be held pending on THAT specific thread -- so the thread's own sigwait/
// sigtimedwait dequeues it (or it is delivered when the thread unblocks) -- rather than being dropped into
// the process-wide g_pending, where any thread (often the sender) could consume it. That misrouting is what
// left a pthread_kill()+sigwait target hung / a peer thread waking instead of the addressed one.
static int thread_tid_blocks_signal(int tid, int sig) {
    if (sig < 1 || sig > 64) return 0;
    int blocked = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c && cpu_tid(g_threg[i].c) == tid) {
            blocked = (g_threg[i].c->sigmask & (1ull << (sig - 1))) != 0;
            break;
        }
    pthread_mutex_unlock(&g_threg_m);
    return blocked;
}

// Is `tid` a LIVE guest thread of this process? tkill/tgkill (syscall/signal.c) use it to return ESRCH for
// a tid no thread carries -- e.g. a joined/exited thread whose id LTP tgkill03 reuses ("defunct tid"). The
// process shares one thread-group, so a tid absent from the registry is gone. (The caller's own tid is
// checked separately at the call site: it is always live even if not yet enumerated here.)
static int thread_tid_alive(int tid) {
    int alive = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c && cpu_tid(g_threg[i].c) == tid) {
            alive = 1;
            break;
        }
    pthread_mutex_unlock(&g_threg_m);
    return alive;
}

// Count of currently-live guest threads of THIS process (main + every spawned one still in run_guest). The
// /proc/<self>/task st_nlink synth reports 2 + this (Linux: `.`, `..`, one subdir per thread) so guest
// sandbox `IsSingleThreaded` (fstatat st_nlink == 3) and per-tid `IsThreadPresentInProcFS` (fstatat ENOENT
// on a joined/exited thread) both track the real thread set -- otherwise a process's thread helpers
// spins 30 iterations waiting for a stopped thread's /proc/self/task/<tid> to disappear, then FATALs.
static int thread_live_count(void) {
    int n = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX; i++)
        if (g_threg[i].c) n++;
    pthread_mutex_unlock(&g_threg_m);
    return n < 1 ? 1 : n; // the caller is always itself a live thread even in a registration window
}

// Fill `out` with the tids of every live guest thread of THIS process (the registered spawned threads;
// the main/init thread carries tid 0 in the registry and reports `main_tid` to the guest, so callers pass
// their own pid as main_tid to substitute it). Returns the count written (<= max). Used to enumerate
// /proc/<self>/task so a directory walk sees every live TID, not just the main thread.
static int thread_tid_list(int *out, int max, int main_tid) {
    int n = 0;
    pthread_mutex_lock(&g_threg_m);
    for (int i = 0; i < THREAD_REG_MAX && n < max; i++) {
        if (!g_threg[i].c) continue;
        int tid = cpu_tid(g_threg[i].c);
        if (tid == 0) tid = main_tid; // init thread -> its guest-visible tid (== pid)
        int dup = 0;
        for (int j = 0; j < n; j++)
            if (out[j] == tid) {
                dup = 1;
                break;
            }
        if (!dup) out[n++] = tid;
    }
    pthread_mutex_unlock(&g_threg_m);
    return n;
}

// execve makes the process single-threaded: the kernel terminates every OTHER thread in the group before the
// new image runs. The JIT re-loads the new image IN-PROCESS, so we must do that teardown by hand -- BEFORE
// flushing the address space and closing CLOEXEC fds -- or a surviving sibling M keeps running the old image
// against freed state (e.g. Go's netpoller M, parked in epoll_wait, crashes with EBADF the instant execve
// closes its epoll fd; every postgres/mysql/mariadb entrypoint `exec gosu ...` right past a Go all-threads
// setuid, so this is on the DB showcase path). Flag every peer exited, wake a futex-parked one and interrupt
// any other blocking host syscall (or nudge one running translated code), and BLOCK until all peers have left
// run_guest and unregistered -- only then is it safe for the caller to munmap/flush the shared address space.
static void thread_exit_others(struct cpu *self) {
    struct timespec slice = {0, 500000};          // 0.5ms between rounds; re-signal each round to catch a peer that was
    for (int round = 0; round < 20000; round++) { // between syscalls when we first flagged it (~10s ceiling)
        int others = 0;
        pthread_mutex_lock(&g_threg_m);
        for (int i = 0; i < THREAD_REG_MAX; i++) {
            struct cpu *tc = g_threg[i].c;
            if (!tc || tc == self) continue;
            others++;
            __atomic_store_n(&tc->exited, 1, __ATOMIC_SEQ_CST);
            pthread_cond_t *cnd = __atomic_load_n(&g_threg[i].waitc, __ATOMIC_SEQ_CST);
            if (cnd) { // parked in a futex wait -> wake it (see thread_target_signal)
                pthread_mutex_t *m = g_threg[i].waitm;
                pthread_mutex_lock(m);
                pthread_cond_broadcast(cnd);
                pthread_mutex_unlock(m);
            }
            pthread_kill(g_threg[i].th, THREAD_INT_SIG); // EINTR any other blocking host syscall
        }
        pthread_mutex_unlock(&g_threg_m);
        if (!others) return;
        nanosleep(&slice, NULL);
    }
}

// ---- robust futex list (set_robust_list / thread-exit cleanup) ----------------------------------------
// A thread that dies while holding a robust mutex must not wedge its waiters forever: the kernel walks the
// thread's registered robust list on exit and, for each futex it still owns, sets FUTEX_OWNER_DIED and wakes
// one waiter so a blocked peer returns EOWNERDEAD (and can pthread_mutex_consistent the lock) instead of
// hanging. hl did neither (set_robust_list was a no-op), so a crash/exit under a PTHREAD_MUTEX_ROBUST lock
// deadlocked every waiter. Layout (LP64 kernel/glibc ABI, 24-byte head):
//   struct robust_list_head { void *list; long futex_offset; void *list_op_pending; };
// `list` chains each mutex's embedded robust_list node (first word = next; LSB is a PI flag we mask off) and
// terminates by pointing back at &head->list (== head, list is at offset 0). The futex word for a node is at
// node + futex_offset. list_op_pending covers a mutex mid-(un)lock and is handled once, at the end.
#define HL_ROBUST_LIST_LIMIT 2048

// Robust-list links are guest pointers read directly from guest memory, so they do not pass through the
// syscall dispatcher's pointer translation. A static ET_EXEC can therefore put a low link address in a
// high-mapped list head. Translate each link before validating or dereferencing it; for PIE, heap, stack,
// and mmap pointers this is an identity operation.
static inline uint64_t robust_guest_to_host(uint64_t address) {
    return nonpie_fold(address);
}

static int robust_user_pointer(uint64_t address) {
    return address != 0 && address <= (uint64_t)INTPTR_MAX;
}

static int robust_futex_address(uint64_t entry, long offset, uint64_t *address) {
    uint64_t result;
    if (offset >= 0) {
        if (__builtin_add_overflow(entry, (uint64_t)offset, &result)) return 0;
    } else {
        uint64_t magnitude = (uint64_t)(-(offset + 1)) + 1;
        if (entry < magnitude) return 0;
        result = entry - magnitude;
    }
    if (!robust_user_pointer(result) || result > (uint64_t)INTPTR_MAX - sizeof(uint32_t)) return 0;
    *address = result;
    return 1;
}

static int robust_copy_from(void *destination, uint64_t source, size_t length) {
    hl_logical_vma_pin pin;
    void *host;
    if (!futex_teardown_pin(source, length, HL_LOGICAL_VMA_READ, &pin, &host)) return 0;
    memcpy(destination, host, length);
    hl_logical_vma_unpin(&pin);
    return 1;
}

// If the dying thread still owns *futex_addr, set FUTEX_OWNER_DIED (preserving FUTEX_WAITERS) and wake one
// waiter. cmpxchg-loops so a concurrent lock/unlock on the same word can't clobber the OWNER_DIED marking.
static void robust_handle_death(uint64_t futex_addr, int mytid) {
    hl_logical_vma_pin pin;
    void *host;
    if (!futex_addr || !futex_teardown_pin(futex_addr, 4, HL_LOGICAL_VMA_WRITE, &pin, &host)) return;
    int *w = host;
    int v = __atomic_load_n(w, __ATOMIC_SEQ_CST);
    for (;;) {
        if (((uint32_t)v & HL_FUTEX_TID_MASK) != (uint32_t)mytid) break; // not (or no longer) ours
        int nv = (int)(((uint32_t)v & HL_FUTEX_WAITERS) | HL_FUTEX_OWNER_DIED);
        if (__atomic_compare_exchange_n(w, &v, nv, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)) {
            if ((uint32_t)v & HL_FUTEX_WAITERS) futex_wake_bucket(w, 1, ~0u, 0); // one waiter -> EOWNERDEAD
            break;
        }
        // v was reloaded with the current word by the failed cmpxchg -> re-check ownership and retry
    }
    hl_logical_vma_unpin(&pin);
}

// Walk this thread's robust list (if any) and mark+wake each still-owned mutex. Clears c->robust_list so a
// second call (thread exit then process exit) is a no-op. Every guest pointer is bounds-checked before deref.
static void futex_robust_exit(struct cpu *c) {
    // Like clear-child-tid, Linux's kernel-driven robust-list wake has no FUTEX_PRIVATE_FLAG and uses the
    // shared key class. Keep it aligned with glibc's robust owner-death wait path.
    g_fbk_active = g_fbk;
    // The head is a guest pointer like every list entry below it (set_robust_list stores it unfolded).
    // Left raw, an in-image head simply failed robust_copy_from and the whole walk was silently skipped --
    // and the `entry == head` wrap test compared a folded entry against an unfolded head.
    uint64_t head = robust_guest_to_host(c->robust_list);
    c->robust_list = 0;
    uint8_t head_copy[24];
    if (!head || !robust_copy_from(head_copy, head, sizeof(head_copy))) return;
    uint64_t raw_first, raw_pending;
    long futex_offset;
    memcpy(&raw_first, head_copy, sizeof(raw_first));
    memcpy(&futex_offset, head_copy + 8, sizeof(futex_offset));
    memcpy(&raw_pending, head_copy + 16, sizeof(raw_pending));
    uint64_t pending = robust_guest_to_host(raw_pending & ~1ULL); // head->list_op_pending
    int mytid = cpu_tid(c);
    uint64_t entry = robust_guest_to_host(raw_first & ~1ULL);
    for (int limit = 0; limit < HL_ROBUST_LIST_LIMIT; limit++) {
        if (entry == head) break; // wrapped back to &head->list -> done
        if (!robust_user_pointer(entry)) break;
        uint64_t raw_next;
        if (!robust_copy_from(&raw_next, entry, sizeof(raw_next))) break;
        uint64_t next = robust_guest_to_host(raw_next & ~1ULL); // entry->next
        uint64_t futex_address;
        if (entry != pending && robust_futex_address(entry, futex_offset, &futex_address))
            robust_handle_death(futex_address, mytid);
        entry = next;
    }
    uint64_t pending_futex;
    if (pending != head && robust_user_pointer(pending) && robust_futex_address(pending, futex_offset, &pending_futex))
        robust_handle_death(pending_futex, mytid);
}

static void *thread_trampoline(void *p) {
    struct cpu *child = (struct cpu *)p;
    sentry_thread_enter(child);
    // sets its own TSD, runs to thread exit
    run_guest(child);
    int exit_status = child->exit_code;
    sentry_thread_leave();
    // cgroup pids: task ended
    atomic_fetch_sub(&g_pids_cur, 1);
    acct_publish_tasks(); // update this process's container-wide task-count contribution
    // robust mutexes this thread still holds -> mark OWNER_DIED + wake a waiter (before the join wakeup)
    futex_robust_exit(child);
    // pthread_join waits on this
    futex_wake_addr(child->ctid);
    thread_exec_owner_complete(child, exit_status);
    (void)hl_target_task_event(child, HL_TASK_EVENT_EXIT_THREAD, 0, (uint64_t)cpu_tid(child), 0);
    free(child);
    return NULL;
}

/* Resume every saved non-leader CPU image as a peer host thread. Checkpoint restore has already sanitized
 * host-transient fields while preserving architectural state, signal state, TLS, TID and clear-child-tid. */
static int thread_restore_group(const struct cpu *images, int count, const struct cpu *leader) {
    if (!images || !leader || count < 1) return -EINVAL;
    int peers = 0;
    int highest_tid = leader->tid;
    for (int i = 0; i < count; i++) {
        if (images[i].tid > highest_tid) highest_tid = images[i].tid;
        if (images[i].tid != 0) peers++;
    }
    if (highest_tid > g_next_tid) g_next_tid = highest_tid;
    if (!peers) return 0;
    txln_activate();
    // See spawn_thread: discard single-threaded (barrier-elided) blocks before the restored peers run.
    if (!g_threaded && !G_THREAD_START_FLUSH()) return -EAGAIN;
    g_threaded = 1;
    atomic_store_explicit(&g_ever_threaded, 1, memory_order_release);
    for (int i = 0; i < count; i++) {
        if (images[i].tid == 0) continue;
        struct cpu *child = malloc(sizeof *child);
        if (!child) return -ENOMEM;
        *child = images[i];
        if (sentry_thread_prepare(child) != 0) {
            free(child);
            return -EAGAIN;
        }
        pthread_t thread;
        if (pthread_create(&thread, NULL, thread_trampoline, child) != 0) {
            sentry_thread_cancel(child);
            free(child);
            return -EAGAIN;
        }
        atomic_fetch_add(&g_pids_cur, 1);
        acct_publish_tasks();
        pthread_detach(thread);
    }
    return 0;
}

// Spawn a guest thread sharing this address space. stack_top is the initial sp.
static int spawn_thread(struct cpu *parent, uint64_t flags, uint64_t stack_top, uint64_t tls, uint64_t ptid,
                        uint64_t ctid) {
    // cgroup pids.max -- gated on the CONTAINER-WIDE task count (all engine processes), not just this
    // process's threads, so the limit is one shared budget across the whole process tree.
    if (g_pids_max && acct_pids_total() >= g_pids_max) return -EAGAIN;
    struct cpu *child = malloc(sizeof *child);
    // ENOMEM
    if (!child) return -12;
    *child = *parent;
#if defined(G_MIXED_PROFILE_CLEAR)
    G_MIXED_PROFILE_CLEAR(child);
#endif
    // child sees clone return 0
    G_RET(child) = 0;
    G_SP(child) = stack_top;
    // resume just after the clone svc
    G_THREAD_RESUME(child, parent);
    // §B: child starts with an EMPTY shadow stack (no parent frames)
    G_SHADOW_RESET(child);
    G_SMC_QUEUE_RESET(child);
    /* clone() inherits architectural register state and the signal mask, but
       these fields describe an in-flight engine operation on the parent host
       thread.  Copying them can deliver the parent's synchronous fault to the
       child or resume a BUS/service handoff with stale scratch state. */
    child->irq = 0;
    child->tpending = 0;
    child->tpending_hi = 0;
    child->sig_defer_hi = 0;
    memset(child->sig_defer_hi_stack, 0, sizeof child->sig_defer_hi_stack);
    child->sync_signal = 0;
    child->sync_code = 0;
    child->sync_address = 0;
    child->vdirty = 0;
    child->fault_addr = 0;
    child->bus_ea = 0;
    child->in_service = 0;
    child->exited = 0;
    child->redirect = 0;
#ifdef G_SOFT_STATE_RESET
    G_SOFT_STATE_RESET(child);
#endif
    // CLONE_SETTLS
    if (flags & 0x00080000) G_TLS(child) = tls;
    int tid = __sync_add_and_fetch(&g_next_tid, 1);
    // This thread's gettid() identity (see proc.c case 178): a unique id, distinct from the init's pid 1.
    child->tid = tid;
    if (!hl_target_task_event(parent, HL_TASK_EVENT_CLONE_THREAD, (uint64_t)tid, (uint64_t)cpu_tid(parent), 0)) {
        free(child);
        return -EAGAIN;
    }
    // CLONE_PARENT_SETTID / CLONE_CHILD_SETTID. clone(2)'s tid slots are ordinary guest pointers and a
    // non-PIE guest may hand over a .bss one, so fold for the store (thread.c's rule); c->ctid keeps the
    // guest value, and futex_wake_addr folds again at thread exit.
    if ((flags & 0x00100000) && ptid) *(int *)nonpie_fold(ptid) = tid;
    if ((flags & 0x01000000) && ctid) *(int *)nonpie_fold(ctid) = tid;
    // CLONE_CHILD_CLEARTID
    child->ctid = (flags & 0x00200000) ? ctid : 0;
    // robust list is per-thread and NOT inherited: a new thread starts empty and re-registers via
    // set_robust_list itself (otherwise the copied parent head would be walked twice on exit).
    child->robust_list = 0;
    // A new CLONE_VM thread starts with no alternate signal stack (Linux
    // sigaltstack(2)); it installs its own stack after startup when needed.
    child->alt_sp = 0;
    child->alt_size = 0;
    child->alt_flags = 2; /* SS_DISABLE */
    // A peer thread may self-modify code; arm eager line-set recording (and back-fill the lines of every
    // block translated so far) NOW, while still single-threaded, so the set is complete before any peer runs.
    txln_activate();
    // 0->1 transition: while STILL single-threaded (no peer exists yet), flush the code cache so any block
    // translated under the single-threaded x86-TSO-barrier-elision regime is discarded and re-translated
    // WITH barriers before this new peer can execute a guest memory op. Only on the transition -- a later
    // clone (g_threaded already 1) must NOT reset the arena in place under live peers. See emit.c /
    // hl_x86_flush_for_thread_start.
    if (!g_threaded && !G_THREAD_START_FLUSH()) {
        (void)hl_target_task_event(child, HL_TASK_EVENT_EXIT_THREAD, 0, (uint64_t)tid, 0);
        free(child);
        return -EAGAIN;
    }
    g_threaded = 1;
    // Publish threading authority before sentry preparation and pthread_create.
    // A concurrent vfork must never mistake an authorized, not-yet-registered
    // peer for a single-threaded process and import a stale COW snapshot.
    atomic_store_explicit(&g_ever_threaded, 1, memory_order_release);
    if (sentry_thread_prepare(child) != 0) {
        (void)hl_target_task_event(child, HL_TASK_EVENT_EXIT_THREAD, 0, (uint64_t)tid, 0);
        free(child);
        return -EAGAIN;
    }
    pthread_t th;
    if (pthread_create(&th, NULL, thread_trampoline, child) != 0) {
        sentry_thread_cancel(child);
        (void)hl_target_task_event(child, HL_TASK_EVENT_EXIT_THREAD, 0, (uint64_t)tid, 0);
        free(child);
        return -EAGAIN;
    }
    // cgroup pids: task created
    atomic_fetch_add(&g_pids_cur, 1);
    acct_publish_tasks(); // update this process's container-wide task-count contribution
    pthread_detach(th);
    return tid;
}
