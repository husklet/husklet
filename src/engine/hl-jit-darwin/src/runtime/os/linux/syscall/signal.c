// Extracted from service(): Signals syscalls. Returns 1 if nr was handled, 0 otherwise. Included by service.c
// after service/helpers.c, before service() — same TU scope (globals + helpers).
// Linux sigaction sa_flags bit (asm-generic, shared by x86-64 + aarch64): a handler installed with
// SA_RESTART asks the kernel to transparently restart a slow syscall it interrupts, rather than failing
// it with EINTR. We record the flag in g_sigact[sig].flags (rt_sigaction, case 134) and consult it here.
#define SA_RESTART_L 0x10000000ull

// Decide whether an interruptible host syscall that just returned EINTR (a host signal fired and
// host_sigh raised a g_pending bit) should be auto-restarted for the guest. POSIX rule: restart iff
// EVERY signal that is pending-and-deliverable now (has a real guest handler and is not blocked by the
// thread mask) was installed with SA_RESTART; if any such handler lacks SA_RESTART the guest must see
// EINTR. With nothing deliverable pending (a SIG_DFL/IGN that host_sigh already actioned, or a spurious
// EINTR) we restart too -- there is no SA_RESTART-less handler whose contract we'd be breaking. The
// awaited handler stays pending and is delivered by the dispatcher's maybe_deliver_signal once the
// restarted syscall finally returns.
static int syscall_should_restart(struct cpu *c) {
    if (ckpt_pending()) return 0; // a whole-tree checkpoint was requested: return EINTR so this process reaches
                                  // its dispatcher safepoint (ckpt_poll) instead of transparently re-blocking
    if (__atomic_load_n(&c->exited, __ATOMIC_SEQ_CST)) return 0; // execve teardown: don't re-block, unwind out
    // Process-wide pending (g_pending) AND this thread's directed-pending (c->tpending, set by tkill/tgkill):
    // a thread blocked in read/accept/recv must be interrupted by a thread-directed signal too, not only a
    // process one. For each deliverable-now signal whose guest handler lacks SA_RESTART, return 0 (EINTR).
    uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
    for (int s = 1; s <= 64; s++) {
        uint64_t bit = 1ull << s;
        if (!(p & bit)) continue;
        if (c->sigmask & (1ull << (s - 1))) continue; // blocked -> not delivered now
        if (g_sigact[s].handler <= 1) continue;       // SIG_DFL/IGN -> no guest handler runs
        if (!(g_sigact[s].flags & SA_RESTART_L)) return 0;
    }
    return 1;
}

// An interruptible host syscall failed: should the caller retry it? True iff it was interrupted (EINTR)
// by a signal whose guest handler asked for SA_RESTART (syscall_should_restart). Use as the tail of a
// do/while around the blocking host call so the result variable stays local to each call site.
#define SVC_EINTR_RESTART(c) (errno == EINTR && syscall_should_restart(c))

// ---- EINTR for a BLOCKING "never-restarted" syscall (poll/ppoll/pselect/epoll_pwait) ----
// Per signal(7) these calls are in the set that is NEVER restarted: when a signal HANDLER interrupts them
// they ALWAYS return EINTR, regardless of SA_RESTART -- the handler runs and the syscall does not restart.
// The old SVC_EINTR_RESTART do/while got this wrong: whenever a pending handler had SA_RESTART it restarted
// the host call IN PLACE, which (a) is the wrong semantics (they must return EINTR) and, worse, (b) never
// delivered the handler because it never returned to the dispatcher -- so a forever-blocking call
// (pause->ppoll(NULL,0,NULL), poll(NULL,0,-1)) plus an SA_RESTART SIGCHLD reaper hung forever.
//
// Correct rule: keep retrying in place ONLY for a SPURIOUS EINTR with nothing to deliver -- an internal/host
// wakeup, or a SIG_DFL/IGN the host already actioned, which a real kernel would not surface to the guest at
// all. The instant a real guest handler is runnable, STOP looping and let the syscall return -EINTR; the
// dispatcher's maybe_deliver_signal then runs the handler (after the syscall returns) and the guest sees
// EINTR -- exactly like Linux. Returns 1 to RETRY the host call, 0 to let it return.
static int svc_poll_retry(struct cpu *c) {
    if (errno != EINTR) return 0;                                // a genuine error -> let it propagate
    if (ckpt_pending()) return 0;                                // checkpoint requested: return EINTR -> safepoint
    if (__atomic_load_n(&c->exited, __ATOMIC_SEQ_CST)) return 0; // execve teardown: stop re-blocking, unwind out
    uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
    for (int s = 1; s <= 64; s++) {
        if (!(p & (1ull << s))) continue;
        if (c->sigmask & (1ull << (s - 1))) continue; // blocked -> not delivered now
        if (g_sigact[s].handler > 1) return 0;        // a runnable guest handler -> return EINTR + deliver it
    }
    return 1; // nothing deliverable -> hide this EINTR and re-block (spurious/internal wakeup)
}

// Route a thread-directed signal (tkill/tgkill at `tid`). If it names ANOTHER live thread and the signal
// has a real guest handler, deliver it to exactly that thread (its cpu->tpending) -- a process-wide
// g_pending would let any thread (typically the sender) consume it, which breaks Go's stop-the-world
// preemption (sysmon tgkill's a worker with SIGURG and must stop THAT worker). Otherwise -- self-signal,
// default/ignore disposition, or an unknown/dead tid -- fall back to the existing process-directed path on
// the caller (raise_guest_signal applies the default/ignore action or coalesces into g_pending as before).
static void thread_kill(struct cpu *c, int tid, int sig) {
    if (sig >= 1 && sig <= 64 && tid != cpu_tid(c) && g_sigact[sig].handler > 1 && thread_target_signal(tid, sig))
        return;
    raise_guest_signal(c, sig);
}

static int svc_signal(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                      uint64_t a5) {
    switch (nr) {
    // ===================== Signals — Linux signal numbers -> macOS; kill/sigaction/sigreturn =====================
    // kill(pid,sig)
    case 129: {
        // Linux kill() validates the signal number FIRST: 0 (an existence probe) and 1..64 are legal, any
        // other value is -EINVAL (kill03: kill(self, 2000) must fail EINVAL, not be swallowed by the self
        // path below). This gate precedes the self/own-group/cross-process routing so every path agrees.
        if ((int)a1 < 0 || (int)a1 > 64) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // checkpoint restore: a kill naming a checkpoint-time guest pid/pgid must reach the live host process
        // the tree was re-forked with (identity no-op on a normal launch, g_pidmap_n==0). Self / own-group /
        // broadcast (a0 == self, 0, -1) are left untranslated so the self path below still matches.
        if (g_pidmap_n && (int)a0 > 0 && (int)a0 != container_pid()) a0 = (uint64_t)(unsigned)pidmap_to_live((int)a0);
        else if (g_pidmap_n && (int)a0 < -1) a0 = (uint64_t)(int64_t)(-pidmap_to_live(-(int)a0));
        if ((int)a0 == 0) {
            // kill(0, sig): Linux signals EVERY process in the CALLER's process group. hl shares its host
            // session/process-group with the launcher + sibling engines, so a raw kill(-getpgrp()) would
            // escape the "container"; instead deliver to each container-registry MEMBER that shares our host
            // process group (which mirrors the guest's -- setpgid is forwarded, case 154), plus ourselves via
            // the in-process self path. The old code only ever signalled the caller, so job-control shells /
            // supervisors / group-shutdown logic left sibling children of the group running (kill_zero
            // child_got=0 vs native 1). sig 0 is a group existence/permission probe -> report success only.
            if ((int)a1 != 0) {
                if (g_init_hostpid) container_group_kill(getpgrp(), sig_l2m((int)a1), (int)getpid());
                raise_guest_signal(c, (int)a1);
            }
            G_RET(c) = 0;
        } else if ((int)a0 == container_pid() || (int)a0 == -1) {
            // SELF (kill(self,sig)) or broadcast (kill(-1)): deliver via our own machinery. Keeping these on
            // the in-process self path is safe (see the kill(0) note above on not escaping to the launcher)
            // and matches the raise/abort self-signal intent.
            raise_guest_signal(c, (int)a1);
            G_RET(c) = 0;
        } else if ((int)a0 < -1) {
            // kill(-pgid, sig): signal a SPECIFIC process group. hl runs each guest process as a real host
            // process whose process group MIRRORS the guest's (case 154 forwards setpgid to the host; non-
            // init children carry their real host pids), so the named group is a real, isolated host group
            // -- route the signal there. The old code folded EVERY a0<=0 into raise_guest_signal (signal
            // MYSELF), so a parent tearing down a child's private group -- LTP SAFE_FORK cleanup does
            // kill(-child_pgid, SIGKILL); likewise node/erlang/posix_spawn teardown -- SIGKILLed its OWN
            // process instead: the parent died 255 with the child's results unreported (all TPASS printed,
            // then rc=255 vs native 0). A member of the target group that is the caller receives the signal
            // back through host_sigh, same as any other cross-process delivery. pgid 1 is the container
            // init <-> its real host group leader.
            pid_t gpgid = (pid_t)(-(int)a0);
            if (gpgid == 1 && g_init_hostpid) gpgid = g_init_hostpid;
            G_RET(c) = kill(-gpgid, sig_l2m((int)a1)) < 0 ? (uint64_t)(-errno) : 0;
        } else
        // Cross-process: the target is another hl engine whose host_sigh is installed on the MACOS signal
        // number (rt_sigaction, case 134, installs on sig_l2m(sig)); its host_sigh translates back via
        // sig_m2l. So the sender MUST translate Linux->macOS here too -- else a divergent signal (SIGUSR1=10,
        // SIGUSR2=12, SIGURG=23, ... differ between Linux and macOS) lands on the wrong disposition and is
        // lost. This is exactly the postgres fast-shutdown deadlock: the postmaster's kill(checkpointer,
        // SIGUSR2=12) was delivered as macOS 12 (SIGSYS), the checkpointer never ran ShutdownXLOG, and
        // `pg_ctl -w stop` hung ("server does not shut down"). sig 0 (existence check) maps to 0 unchanged.
        // The container init is guest pid 1 <-> its real host pid (g_init_hostpid): a sibling process that
        // kill()s pid 1 must reach the init's host process, not host pid 1 (launchd).
        {
            pid_t tgt = ((int)a0 == 1 && g_init_hostpid) ? g_init_hostpid : (pid_t)a0;
            // guest-pid namespace: a container may only signal a process INSIDE itself. Reject a target that
            // is not a live member of this container's process registry -> ESRCH. This closes the host-pid
            // authority leak: without it a guest kill(2)/kill(pid,0) could reach an ARBITRARY same-user host
            // pid -- a sibling engine (another container), the launcher, or any of the hl user's processes.
            // Legitimate cross-guest-process signalling still works (a real peer IS a registry member). Gated
            // on container mode (g_init_hostpid); bare (non-container) mode keeps the historical host model.
            if (g_init_hostpid && !container_host_member((int)tgt)) {
                G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
                break;
            }
            G_RET(c) = kill(tgt, sig_l2m((int)a1)) < 0 ? (uint64_t)(-errno) : 0;
        }
        break;
    }
    case 130: {
        // tkill(tid, sig). Linux rejects tid <= 0 and an out-of-range signal with EINVAL, then ESRCH if no
        // live thread carries that tid. (raise() lowers to tgkill on modern glibc; keep tkill correct too.)
        int tid = (int)a0, sig = (int)a1;
        if (tid <= 0 || sig < 0 || sig > 64) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (tid != cpu_tid(c) && !thread_tid_alive(tid)) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        thread_kill(c, tid, sig);
        G_RET(c) = 0;
        break;
    }
    case 131: {
        // tgkill(tgid, tid, sig). Linux validation, in order (tgkill03):
        //   tgid <= 0 || tid <= 0 || sig out of [0,64] -> EINVAL;
        //   otherwise the thread `tid` must be live AND belong to thread-group `tgid` -> else ESRCH.
        // The whole guest process is one thread-group (container_pid()), so a tgid that names anything else
        // (tgkill03 "Defunct tgid") or a tid no live thread carries ("Defunct tid") is ESRCH.
        int tgid = (int)a0, tid = (int)a1, sig = (int)a2;
        if (tgid <= 0 || tid <= 0 || sig < 0 || sig > 64) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        if (tgid != container_pid() || (tid != cpu_tid(c) && !thread_tid_alive(tid))) {
            G_RET(c) = (uint64_t)(int64_t)(-ESRCH);
            break;
        }
        thread_kill(c, tid, sig);
        G_RET(c) = 0;
        break;
    }
    case 138: { // rt_sigqueueinfo(tgid, sig, siginfo): carry si_code + si_value to the handler's siginfo
        int sig = (int)a1;
        if (sig >= 1 && sig <= 64 && a2) {
            g_sigcode[sig] = *(int *)(a2 + 8);      // siginfo.si_code
            g_sigval[sig] = *(uint64_t *)(a2 + 24); // siginfo.si_value (sival_int/ptr)
        }
        raise_guest_signal(c, sig);
        G_RET(c) = 0;
        break;
    }
    // sigaltstack(new, old): struct sigaltstack { void *ss_sp; int ss_flags; size_t ss_size; } (24 bytes).
    case 132: {
        // Linux validates BEFORE writing `old`: bad pointers -> EFAULT; an unknown ss_flags mode -> EINVAL;
        // and, unless SS_DISABLE, a stack smaller than MINSIGSTKSZ -> ENOMEM. Without this hl installs a
        // bogus/tiny altstack that corrupts later SA_ONSTACK signal delivery.
        if ((a1 && guest_bad_ptr(a1, 24)) || (a0 && guest_bad_ptr(a0, 24))) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (a0) {
            uint32_t nflags = *(uint32_t *)(a0 + 8);
            uint64_t nsize = *(uint64_t *)(a0 + 16);
            // valid set-flag bits: SS_ONSTACK(1), SS_DISABLE(2), SS_AUTODISARM(0x80000000).
            if (nflags & ~(uint32_t)(1u | 2u | 0x80000000u)) {
                G_RET(c) = (uint64_t)(-EINVAL);
                break;
            }
            if (!(nflags & 2u) && nsize < 2048 /* MINSIGSTKSZ */) {
                G_RET(c) = (uint64_t)(-ENOMEM);
                break;
            }
        }
        if (a1) {
            // report current (or SS_DISABLE=2 if none)
            *(uint64_t *)(a1 + 0) = c->alt_sp;
            *(uint32_t *)(a1 + 8) = c->alt_sp ? c->alt_flags : 2;
            *(uint64_t *)(a1 + 16) = c->alt_size;
        }
        if (a0) {
            c->alt_sp = *(uint64_t *)(a0 + 0);
            c->alt_flags = *(uint32_t *)(a0 + 8);
            c->alt_size = *(uint64_t *)(a0 + 16);
        }
        G_RET(c) = 0;
        break;
    }
#if G_O_DIRECTORY == 0x10000
    // x86-64 pause(34): wait until a signal bearing a guest handler (unblocked under the CURRENT mask) is
    // pending, then return -EINTR (the dispatcher delivers the handler). It has no aarch64 syscall number --
    // asm-generic libcs lower pause() to ppoll(NULL,0,NULL) (handled by case 73), but x86-64 glibc issues the
    // real pause syscall, which arrived UNMAPPED (CANON_X86ONLY|34) and ENOSYS'd -> a guest pause() returned
    // immediately instead of blocking. pause == rt_sigsuspend with the current mask (no mask change); reuse
    // that exact deliver-in-the-dispatcher discipline (case 133) rather than block on a host syscall.
    case 0x10000 | 34: {
        sigset_t allblk, prev, empty;
        sigfillset(&allblk);
        sigemptyset(&empty);
        sigprocmask(SIG_BLOCK, &allblk, &prev); // close the check/sleep race (see case 133)
        ts_wait_enter();                        // pause -> interruptible sleep ('S') until a deliverable signal arrives
        while (!c->exited) {
            uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST);
            int deliv = 0;
            for (int s = 1; s <= 64; s++) {
                uint64_t bit = 1ull << s;
                if (!(p & bit) || (c->sigmask & (1ull << (s - 1)))) continue; // not pending / blocked
                if (g_sigact[s].handler <= 1) { // SIG_DFL/IGN: host already actioned -> consume, keep waiting
                    __atomic_and_fetch(&g_pending, ~bit, __ATOMIC_SEQ_CST);
                    continue;
                }
                deliv = 1; // a real guest handler is runnable -> stop waiting (leave it pending to deliver)
                break;
            }
            if (deliv) break;
            sigsuspend(&empty); // sleep until any host signal (host_sigh sets g_pending); EINTR-returns
        }
        ts_wait_leave();
        sigprocmask(SIG_SETMASK, &prev, NULL);
        G_RET(c) = (uint64_t)(-EINTR);
        break;
    }
#endif
    // rt_sigsuspend(const sigset_t *unewset, size_t sigsetsize): atomically install the guest's arg
    // mask, wait until a signal that has a guest handler (and is unblocked under that mask) becomes
    // pending, then return -EINTR -- the handler runs and only then does sigsuspend "return" (standard
    // semantics). c->sigmask is a guest sigset_t (bit signo-1); g_pending is 1<<signo.
    //
    // We do NOT build the signal frame here: delivery is left to the dispatcher's maybe_deliver_signal,
    // which fires AFTER the per-arch pc advance past the syscall (x86 pre-advances rip, aarch64 does
    // pc+=4 post-service) -- building a frame inline would re-execute the SVC on aarch64. So we leave the
    // awaited signal pending and arrange c->sigmask so the dispatcher delivers it, then restore the
    // pre-suspend mask (minus the one awaited bit, which must stay unblocked for that delivery; that one
    // bit is the only deviation from a perfect mask restore).
    case 133: {
        // The kernel copy_from_user()s the new mask, so a bad set pointer is -EFAULT (LTP sigsuspend02's
        // tst_get_bad_addr case). That address is a guest PROT_NONE guard page (physically R+W under hl but
        // faulting per Linux), tracked in the g_gna registry -- gna_hit catches it WITHOUT a probe-read, so a
        // valid but non-host-mapped non-PIE .bss sigset (this handler reads a0 directly, unrebased) is not
        // mistaken for a fault. NULL is not a valid rt_sigsuspend mask -> EFAULT too.
        if (a0 == 0 || gna_hit((uint64_t)a0, 8)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint64_t oldmask = c->sigmask;
        uint64_t newmask = *(uint64_t *)a0;
        c->sigmask = newmask;
        // Block all host signals around the pending check so host_sigh cannot fire between the check and
        // the sleep (lost-wakeup race); sigsuspend(&empty) then atomically unblocks + waits.
        sigset_t allblk, prev, empty;
        sigfillset(&allblk);
        sigemptyset(&empty);
        sigprocmask(SIG_BLOCK, &allblk, &prev);
        ts_wait_enter(); // rt_sigsuspend -> interruptible sleep ('S')
        int deliv = 0;
        while (!c->exited) {
            uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST);
            deliv = 0;
            for (int s = 1; s <= 64; s++) {
                uint64_t bit = 1ull << s;
                if (!(p & bit) || (newmask & (1ull << (s - 1)))) continue; // not pending / blocked
                uint64_t h = g_sigact[s].handler;
                if (h <= 1) { // SIG_DFL/IGN: host already actioned it -> consume, keep waiting
                    __atomic_and_fetch(&g_pending, ~bit, __ATOMIC_SEQ_CST);
                    continue;
                }
                deliv = s; // a real guest handler is runnable -> stop waiting (leave it PENDING)
                break;
            }
            if (deliv) break;
            sigsuspend(&empty); // sleep until any host signal (host_sigh sets g_pending); EINTR-returns
        }
        ts_wait_leave();
        sigprocmask(SIG_SETMASK, &prev, NULL); // restore the host signal mask
        // Restore the EXACT pre-suspend mask (POSIX: sigsuspend restores the caller's original mask on
        // return). The awaited signal is delivered by the dispatcher AFTER this returns -EINTR; when the
        // restored mask BLOCKS that signal (LTP sigsuspend01 blocks SIGALRM around sigsuspend(empty)),
        // clearing its bit out of c->sigmask would corrupt the mask the sigframe saves+restores -- so leave
        // c->sigmask = oldmask and force just that one delivery via g_force_deliver (mask stays intact).
        c->sigmask = oldmask;
        if (deliv) g_force_deliver |= (1ull << deliv);
        G_RET(c) = (uint64_t)(-EINTR);
        break;
    }
    // rt_sigtimedwait(const sigset_t *set, siginfo_t *info, const struct timespec *timeout, size_t):
    // SYNCHRONOUSLY dequeue one pending signal from `set` (no handler runs) and return its signo, or
    // -EAGAIN on timeout. Poll g_pending against `set` in short slices (the in-process model has no
    // single host primitive that covers both host-delivered and raise_guest_signal-injected pendings).
    case 137: {
        uint64_t set = a0 ? *(uint64_t *)a0 : 0; // guest sigset_t (bit signo-1)
        struct timespec *to = (struct timespec *)a2;
        // negative/zero timeout -> single non-blocking poll; else a deadline.
        long long budget_ns = to ? (long long)to->tv_sec * 1000000000LL + to->tv_nsec : -1;
        long long waited_ns = 0;
        // An awaited signal with NO guest handler is invisible to g_pending unless a host handler catches it:
        // a cross-process (or host) kill(2) would otherwise hit the default disposition and terminate us
        // instead of being consumed synchronously here (this is what made a plain sigwait() fail). Install the
        // engine's SA_SIGINFO host handler on each such awaited signal's macOS number so it becomes pending,
        // then restore the prior disposition after the wait. (Signals that already have a guest handler route
        // to g_pending via that handler's host_sigh_si; unmaskable/synchronous signals are skipped.)
        struct sigaction saved[65];
        uint64_t installed = 0;
        for (int s = 1; s <= 64; s++) {
            if (!(set & (1ull << (s - 1))) || s == 9 || s == 19 || sig_is_sync(s)) continue;
            if (g_sigact[s].handler > 1) continue;
            struct sigaction sa;
            memset(&sa, 0, sizeof sa);
            sa.sa_sigaction = host_sigh_si;
            sa.sa_flags = SA_SIGINFO;
            sigfillset(&sa.sa_mask);
            if (sigaction(sig_l2m(s), &sa, &saved[s]) == 0) installed |= (1ull << s);
        }
        int got = 0;
        ts_wait_enter(); // rt_sigtimedwait blocks in interruptible sleep ('S') until a signal/timeout
        for (;;) {
            // Both queues: process-directed (g_pending) and thread-directed (tpending via tkill/tgkill).
            uint64_t p =
                __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
            for (int s = 1; s <= 64; s++)
                if ((p & (1ull << s)) && (set & (1ull << (s - 1)))) {
                    got = s;
                    break;
                }
            if (got) {
                __atomic_and_fetch(&g_pending, ~(1ull << got), __ATOMIC_SEQ_CST); // dequeue from both
                __atomic_and_fetch(&c->tpending, ~(1ull << got), __ATOMIC_SEQ_CST);
                if (a1) { // fill siginfo_t whenever info != NULL (a3 is the sigsetsize, not a size threshold)
                    memset((void *)a1, 0, 128);
                    *(int *)(a1 + 0) = got;            // si_signo
                    *(int *)(a1 + 8) = g_sigcode[got]; // si_code
                    if (g_sigpid[got]) {
                        *(int *)(a1 + 16) = g_sigpid[got];
                        *(int *)(a1 + 20) = g_siguid[got];
                    }
                    *(uint64_t *)(a1 + 24) = g_sigval[got];
                    g_sigcode[got] = 0;
                    g_sigval[got] = 0;
                    g_sigpid[got] = 0;
                    g_siguid[got] = 0;
                }
                G_RET(c) = (uint64_t)got;
                break;
            }
            if (budget_ns == 0 || (budget_ns > 0 && waited_ns >= budget_ns) ||
                __atomic_load_n(&c->exited, __ATOMIC_SEQ_CST)) {
                G_RET(c) = (uint64_t)(-EAGAIN);
                break;
            }
            struct timespec slice = {0, 2 * 1000 * 1000}; // 2ms
            nanosleep(&slice, NULL);
            waited_ns += 2 * 1000 * 1000;
        }
        ts_wait_leave();
        for (int s = 1; s <= 64; s++)
            if (installed & (1ull << s)) sigaction(sig_l2m(s), &saved[s], NULL); // restore disposition
        break;
    }
    // rt_sigaction(sig, *act, *old)
    case 134: {
        int sig = (int)a0;
        if (sig < 1 || sig > 64) {
            G_RET(c) = (uint64_t)(-22);
            break;
        }
        // SIGKILL(9) and SIGSTOP(19) can never have their disposition changed -- rt_sigaction returns
        // -EINVAL for them REGARDLESS of act/oldact (LTP signal01/signal02: signal(SIGKILL/SIGSTOP, h)
        // must fail EINVAL; a paused child that "installed" a SIGKILL handler must still be killed by it).
        // The old code fell through and recorded g_sigact[9/19].handler, so a later kill(SIGKILL) tried to
        // run a guest handler instead of terminating. Reject before touching act/oldact, exactly like the
        // kernel (which validates sig before copy_from_user of act).
        if (sig == 9 || sig == 19) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        // The act/oldact structs are read/written DIRECTLY by the engine (24 bytes: handler,flags,mask), so
        // a bad/unmapped pointer must return -EFAULT rather than fault the engine. Validate in Linux
        // order -- copyin `act` (a1) before copyout `oldact` (a2) -- so no oldact is written when act faults.
        if (a1 && !host_range_mapped((uintptr_t)a1, 24)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a2 && !host_range_mapped((uintptr_t)a2, 24)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a2) {
            *(uint64_t *)(a2 + 0) = g_sigact[sig].handler;
            *(uint64_t *)(a2 + 8) = g_sigact[sig].flags;
            *(uint64_t *)(a2 + 16) = g_sigact[sig].mask;
            // aarch64: handler,flags,mask
        }
        if (a1) {
            uint64_t h = *(uint64_t *)(a1 + 0);
            g_sigact[sig].handler = h;
            g_sigact[sig].flags = *(uint64_t *)(a1 + 8);
            g_sigact[sig].mask = *(uint64_t *)(a1 + 16);
            // Synchronous CPU faults (SIGILL/FPE/TRAP/SEGV/BUS) ALWAYS stay on the engine's own host guard
            // (installed at startup): it intercepts the hardware fault and either delivers it to the guest
            // handler recorded in g_sigact above (deliver_guest_fault) or applies the default action
            // (decline -> re-raise). We therefore never forward the guest's disposition to the real host
            // for these -- doing so would UNINSTALL the guard, so a later CPU-feature probe that traps an
            // unsupported instruction (OpenSSL SM3/SM4 + a SIGILL handler) would fault fatally instead of
            // reaching its handler. The bug surfaced across execve: a non-PIE parent (rustup) restoring
            // SIGILL to SIG_DFL left the host guard uninstalled for the exec'd child (cargo). Only ASYNC
            // signals touch the real host disposition. (SIGKILL/SIGSTOP are unmaskable.)
            if (sig != 9 && sig != 19 && !sig_is_sync(sig)) {
                // host(macOS) signo to install on
                int ms = sig_l2m(sig);
                if (h == 0)
                    signal(ms, SIG_DFL);
                else if (h == 1)
                    // honor SIG_IGN (e.g. SIGPIPE)
                    signal(ms, SIG_IGN);
                else {
                    // async: flag pending, deliver in dispatcher. SA_SIGINFO so host_sigh_si can capture the
                    // sender's si_pid/si_uid for an SA_SIGINFO guest handler.
                    struct sigaction sa;
                    memset(&sa, 0, sizeof sa);
                    sa.sa_sigaction = host_sigh_si;
                    sa.sa_flags = SA_SIGINFO;
                    // SIGCHLD: honor SA_NOCLDSTOP by forwarding it to the host SIGCHLD action (Linux flag bit
                    // NOCLDSTOP=0x1 differs from macOS's value, so translate) -- the host then suppresses the
                    // child-stop SIGCHLD. SA_NOCLDWAIT is NOT forwarded: macOS SA_NOCLDWAIT also SUPPRESSES the
                    // termination SIGCHLD, whereas Linux still delivers it -- so we keep the normal handler and
                    // auto-reap the terminated child inside host_sigh_si instead (see there), which both runs
                    // the guest handler AND leaves no zombie (guest wait() -> ECHILD).
                    if (sig == 17 && (g_sigact[sig].flags & 0x1)) sa.sa_flags |= SA_NOCLDSTOP;
                    sigfillset(&sa.sa_mask);
                    sigaction(ms, &sa, NULL);
                }
            } else if (sig == 4 || sig == 5 || sig == 8) {
                // SIGILL/SIGTRAP/SIGFPE are synchronous-fault signals but, unlike SIGSEGV/SIGBUS, have NO
                // POSIX guard installed (a real illegal instruction reaches the arm64 Mach exception port
                // and x86 #DE is synthesized at the dispatcher). So the ONLY thing a POSIX handler for them
                // ever sees is an EXTERNAL kill(2)/tgkill of the signal -- which Linux delivers as an async
                // signal that must wake pause()/sigsuspend() and run the handler (LTP pause01). Honor the
                // guest disposition on the host: a real handler -> host_sigh_sync (queue pending + wake);
                // SIG_DFL/SIG_IGN -> the host default so an external kill takes the correct default action.
                // (Installing a POSIX handler here does NOT disturb the hardware-fault path above.)
                int ms = sig_l2m(sig);
                if (h == 0)
                    signal(ms, SIG_DFL);
                else if (h == 1)
                    signal(ms, SIG_IGN);
                else {
                    struct sigaction sa;
                    memset(&sa, 0, sizeof sa);
                    sa.sa_sigaction = host_sigh_sync;
                    sa.sa_flags = SA_SIGINFO;
                    sigfillset(&sa.sa_mask);
                    sigaction(ms, &sa, NULL);
                }
            }
        }
        G_RET(c) = 0;
        break;
    }
    // rt_sigprocmask(how, *set, *old, sigsetsize)
    case 135: {
        // (W4F slow-path counter removed: it lived in x86 emit.c, undefined in the shared/aarch64 TU)
        // Linux validates the ABI: sigsetsize must equal sizeof(kernel sigset)=8, and when a `set` is
        // supplied `how` must be SIG_BLOCK/UNBLOCK/SETMASK(0/1/2) -- an unknown `how` is EINVAL, not a
        // silent set-mask. Otherwise malformed mask ops report success and mis-shape the guest mask.
        if ((size_t)a3 != 8) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (a1 && (int)a0 != 0 && (int)a0 != 1 && (int)a0 != 2) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (a2) *(uint64_t *)a2 = c->sigmask;
        if (a1) {
            uint64_t set = *(uint64_t *)a1;
            if (a0 == 0)
                // SIG_BLOCK
                c->sigmask |= set;
            else if (a0 == 1)
                // SIG_UNBLOCK
                c->sigmask &= ~set;
            else
                c->sigmask = set;
            // SIG_SETMASK
            // Linux never lets SIGKILL(9) or SIGSTOP(19) be blocked -- the kernel silently strips them
            // from the new mask. Without this they could sit in c->sigmask and raise_guest_signal would
            // treat a fatal/stopping signal as merely pending, letting the guest survive a SIGKILL.
            c->sigmask &= ~((1ull << (9 - 1)) | (1ull << (19 - 1)));
        }
        // Mirror the terminal-stop signals (SIGTSTP/SIGTTIN/SIGTTOU) onto the REAL host mask. Job control
        // depends on this: bash blocks these three around tcsetpgrp/tcsetattr so a process in a BACKGROUND
        // process group can hand the controlling terminal to a new foreground job without itself being
        // stopped (their default action stops the process). The guest runs IN-PROCESS in the engine, so a
        // guest-only mask is invisible to the kernel -- it would deliver SIG_DFL SIGTTOU and STOP the engine
        // mid-handoff, the tcsetpgrp never completes, and every foreground command freezes (the "no job
        // control / Stopped" bug). Only these three need mirroring (only they stop the process on default
        // disposition); all other signals stay on the engine's async host_sigh + c->sigmask delivery model.
        // Fast path: only touch the host mask when THIS call could change a stop-signal's block state -- i.e.
        // SIG_SETMASK (redefines all) or a set that names one of the three -- so the common SIG_BLOCK/UNBLOCK
        // of SIGCHLD/SIGINT/etc. adds zero host syscalls.
        const uint64_t STOPBITS = (1ull << 19) | (1ull << 20) | (1ull << 21); // SIGTSTP|SIGTTIN|SIGTTOU bits
        if (a1 && (a0 == 2 || (*(uint64_t *)a1 & STOPBITS))) {
            static const int STOPS[3] = {20, 21, 22}; // Linux SIGTSTP, SIGTTIN, SIGTTOU
            sigset_t blk, unblk;
            sigemptyset(&blk);
            sigemptyset(&unblk);
            for (int i = 0; i < 3; i++) {
                int ms = sig_l2m(STOPS[i]);
                if (c->sigmask & (1ull << (STOPS[i] - 1)))
                    sigaddset(&blk, ms);
                else
                    sigaddset(&unblk, ms);
            }
            sigprocmask(SIG_BLOCK, &blk, NULL);
            sigprocmask(SIG_UNBLOCK, &unblk, NULL);
        }
        G_RET(c) = 0;
        break;
    }
    // rt_sigpending(set, sigsetsize)
    case 136: {
        // The kernel copy_to_user()s the pending set, so a bad/unmapped `set` pointer is -EFAULT (LTP
        // sigpending02's tst_get_bad_addr case: a PROT_NONE guard page must fault, not be silently written).
        // guest_bad_ptr (not host_range_mapped) so the PROT_NONE probe page is caught. NULL set faults too.
        if (guest_bad_ptr((uintptr_t)a0, 8)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        // Report BOTH queues (process-directed g_pending + this thread's tpending), matching Linux which
        // unions the shared and per-thread pending sets. Also mask to signals that are currently BLOCKED:
        // an unblocked pending signal has a runnable handler and is about to be delivered, but sigpending's
        // contract is "signals pending AND blocked" -- however Linux actually reports every pending signal
        // regardless of the mask, so union without masking (the caller blocks them before checking).
        uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
        uint64_t out = 0;
        for (int s = 1; s <= 64; s++)
            // 1<<N -> sigset_t bit N-1
            if (p & (1ull << s)) out |= (1ull << (s - 1));
        *(uint64_t *)a0 = out;
        G_RET(c) = 0;
        break;
    }
    case 139:
        do_sigreturn(c);
        c->redirect = 1;
        // rt_sigreturn (restorer path)
        break;
    default: return 0;
    }
    return 1;
}
