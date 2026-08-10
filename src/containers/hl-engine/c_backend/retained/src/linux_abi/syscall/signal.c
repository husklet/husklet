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
    if (ckpt_pending())
        return 0; // a whole-tree checkpoint was requested: return EINTR so this process reaches
                  // its dispatcher safepoint (ckpt_poll) instead of transparently re-blocking
    if (__atomic_load_n(&c->exited, __ATOMIC_SEQ_CST)) return 0; // execve teardown: don't re-block, unwind out
    // Process-wide pending (g_pending) AND this thread's directed-pending (c->tpending, set by tkill/tgkill):
    // a thread blocked in read/accept/recv must be interrupted by a thread-directed signal too, not only a
    // process one. Scan every signal deliverable-now (unblocked, with a real guest handler).
    uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
    int deliverable = 0;    // at least one runnable guest handler is pending
    int all_sa_restart = 1; // ...and every such handler asked for SA_RESTART
    for (int s = 1; s <= 64; s++) {
        uint64_t bit = 1ull << s;
        if (!(p & bit)) continue;
        if (c->sigmask & (1ull << (s - 1))) continue; // blocked -> not delivered now
        if (g_sigact[s].handler <= 1) continue;       // SIG_DFL/IGN -> no guest handler runs
        deliverable = 1;
        if (!(g_sigact[s].flags & SA_RESTART_L)) all_sa_restart = 0;
    }
    // Nothing runnable pending: a spurious/host-actioned EINTR a real kernel would not surface -> re-block the
    // host call transparently in place (the old restart-in-loop behaviour, kept for this case only).
    if (!deliverable) return 1;
    // A runnable guest handler MUST run before the interrupted call proceeds -- Linux runs the handler on EVERY
    // interrupted slow syscall, THEN (for SA_RESTART) restarts it. Restarting in place here never returns to the
    // dispatcher, so maybe_deliver_signal never fires and the handler is stranded until the call finally
    // completes on its own (e.g. `timeout 1 sleep 5`: the SIGALRM handler that must kill the child only ran when
    // the child already exited). So STOP looping and return to the dispatcher. When every runnable handler is
    // SA_RESTART, leave the guest PC on the SVC (c->redirect) so the syscall re-executes AFTER the handler runs
    // (transparent restart); otherwise advance past it and let the guest observe EINTR. Either way the pending
    // handler is delivered by the dispatcher's maybe_deliver_signal at the next block boundary.
    if (all_sa_restart) {
        c->redirect = 1;
        g_syscall_restart = 1; // service() restores the arg0/return-aliased register before re-executing
    }
    return 0;
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
enum { HL_SI_TKILL = -6 };

static void thread_kill(struct cpu *c, int tid, int sig) {
    // Route to exactly the addressed thread's cpu->tpending when it names ANOTHER live thread and the signal
    // is either handled by a guest handler OR blocked by that target (destined for its sigwait / held
    // pending). A process-wide g_pending would let any thread consume it, breaking Go's stop-the-world
    // preemption and thread-directed sigwait alike. Otherwise -- self-signal, default/ignore on an unblocked
    // signal, or an unknown/dead tid -- fall back to the process-directed path on the caller.
    if (sig >= 1 && sig <= 64 && tid != cpu_tid(c) &&
        (g_sigact[sig].handler > 1 || thread_tid_blocks_signal(tid, sig))) {
        // Stamp the tkill/tgkill siginfo the target thread's handler reads: Linux sets si_code == SI_TKILL
        // and si_pid == the sender's thread-group (== this guest pid). glibc's SIGCANCEL handler (used by
        // pthread_cancel) IGNORES any signal whose si_code != SI_TKILL or si_pid != getpid(), so without
        // this an asynchronous pthread_cancel of a compute-bound thread was silently dropped and pthread_join
        // hung forever. Written to the single-slot g_sig* (never g_pending) so only the addressed thread's
        // tpending delivery consumes it; the values are constant for any thread-directed signal.
        g_sigcode[sig] = HL_SI_TKILL;
        g_sigval[sig] = 0;
        g_sigpid[sig] = container_pid();
        g_siguid[sig] = 0;
        if (thread_target_signal(tid, sig)) return;
    }
    // Self-directed (or otherwise process-routed) tkill/tgkill. Linux stamps si_code == SI_TKILL for BOTH
    // syscalls regardless of the target -- glibc's raise() lowers to tgkill, so a signalfd/sigwaitinfo
    // reader of a raise()d signal must see SI_TKILL, not the SI_USER(0) a plain kill(2) carries.
    cred_init();
    raise_guest_signal_si(c, sig, HL_SI_TKILL, 0, container_pid(), g_ruid);
}

static int svc_signal(struct cpu *c, uint64_t nr, uint64_t a0, uint64_t a1, uint64_t a2, uint64_t a3, uint64_t a4,
                      uint64_t a5) {
    if ((nr >= 129 && nr <= 139) || nr == (UINT64_C(0x10000) | 34))
        HL_LOGF(&g_jit_log, HL_LOG_TAG_SIGNAL, "nr=%llu target=%lld signal=%lld", (unsigned long long)nr, (long long)a0,
                (long long)a1);
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
        // the tree was re-forked with (identity no-op on a normal launch when the pid map is empty). Self / own-group /
        // broadcast (a0 == self, 0, -1) are left untranslated so the self path below still matches.
        if (hl_linux_pidmap_count(&g_pidmap) != 0 && (int)a0 > 0 && (int)a0 != container_pid())
            a0 = (uint64_t)(unsigned)hl_linux_pidmap_host(&g_pidmap, (int)a0);
        else if (hl_linux_pidmap_count(&g_pidmap) != 0 && (int)a0 < -1)
            a0 = (uint64_t)(int64_t)(-hl_linux_pidmap_host(&g_pidmap, -(int)a0));
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
    case 138: { // rt_sigqueueinfo(tgid, sig, siginfo): carry si_code + si_value + sender identity, and
                // QUEUE every instance for a realtime signal (glibc sigqueue). The user siginfo already
                // holds si_code (SI_QUEUE), si_pid/si_uid (the sender), and si_value.
        int sig = (int)a1;
        // The kernel copy_from_user()s the whole siginfo before it does anything else, so a bad (or NULL)
        // user pointer is -EFAULT (never a fault in the caller). Guard the direct deref below to match --
        // guest_bad_ptr also catches a guest PROT_NONE guard page, as sigaltstack (case 132) does.
        unsigned char siginfo[128];
        if (guest_copy_from(siginfo, a2, sizeof siginfo) != (ssize_t)sizeof siginfo) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (sig >= 1 && sig <= 64 && a2) {
            int code, pid, uid;
            uint64_t value;
            memcpy(&code, siginfo + 8, sizeof code);
            memcpy(&pid, siginfo + 16, sizeof pid);
            memcpy(&uid, siginfo + 20, sizeof uid);
            memcpy(&value, siginfo + 24, sizeof value);
            raise_guest_signal_si(c, sig, code, value, pid ? pid : container_pid(), uid);
        } else {
            raise_guest_signal(c, sig);
        }
        G_RET(c) = 0;
        break;
    }
    // sigaltstack(new, old): struct sigaltstack { void *ss_sp; int ss_flags; size_t ss_size; } (24 bytes).
    case 132: {
        // Linux validates BEFORE writing `old`: bad pointers -> EFAULT; an unknown ss_flags mode -> EINVAL;
        // and, unless SS_DISABLE, a stack smaller than MINSIGSTKSZ -> ENOMEM. Without this hl installs a
        // bogus/tiny altstack that corrupts later SA_ONSTACK signal delivery.
        unsigned char new_stack[24] = {0};
        if ((a0 && guest_copy_from(new_stack, a0, sizeof new_stack) != (ssize_t)sizeof new_stack) ||
            (a1 && guest_accessible_prefix(a1, 24, HL_LOGICAL_VMA_WRITE) != 24)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        if (a0) {
            uint32_t nflags;
            uint64_t nsize;
            memcpy(&nflags, new_stack + 8, sizeof nflags);
            memcpy(&nsize, new_stack + 16, sizeof nsize);
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
            // report current (or SS_DISABLE=2 if none). When the querying thread's SP currently lies within
            // the configured alt stack -- i.e. sigaltstack(NULL,&old) is called from inside an SA_ONSTACK
            // handler running on it -- Linux ORs in SS_ONSTACK(1); the engine reported a bare 0 before.
            uint32_t flags = c->alt_sp ? c->alt_flags : 2;
            uint64_t sp = G_SP(c);
            if (c->alt_sp && !(flags & 2u) && sp >= c->alt_sp && sp < c->alt_sp + c->alt_size)
                flags |= 1u; // SS_ONSTACK
            unsigned char old_stack[24] = {0};
            memcpy(old_stack, &c->alt_sp, sizeof c->alt_sp);
            memcpy(old_stack + 8, &flags, sizeof flags);
            memcpy(old_stack + 16, &c->alt_size, sizeof c->alt_size);
            if (guest_copy_to(a1, old_stack, sizeof old_stack) != (ssize_t)sizeof old_stack) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (a0) {
            uint64_t nsp, nsize;
            memcpy(&nsp, new_stack, sizeof nsp);
            memcpy(&nsize, new_stack + 16, sizeof nsize);
            if (g_nonpie_lo && nsp >= g_nonpie_lo && nsize <= g_nonpie_hi - nsp &&
                !hl_host_range_mapped((uintptr_t)nsp, (size_t)nsize)) {
                gbus_mapping_transition_lock();
                if (!jit_guest_soft_activate()) {
                    gbus_mapping_transition_unlock();
                    G_RET(c) = (uint64_t)(int64_t)-ENOMEM;
                    break;
                }
                gbus_mapping_stw_begin();
                int mapped = hl_logical_vma_global_map_direct(nsp, nsize, HL_LOGICAL_VMA_READ | HL_LOGICAL_VMA_WRITE,
                                                              nonpie_fold(nsp));
                hl_logical_vma_global_reclaim_quiescent();
                gbus_mapping_stw_end();
                gbus_mapping_transition_unlock();
                if (mapped != 0) {
                    G_RET(c) = (uint64_t)(int64_t)-ENOMEM;
                    break;
                }
            }
            c->alt_sp = nsp;
            memcpy(&c->alt_flags, new_stack + 8, sizeof c->alt_flags);
            c->alt_size = nsize;
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
        //
        // sigsetsize is validated FIRST, before the mask is even copied in: Linux's
        // SYSCALL_DEFINE2(rt_sigsuspend) opens with `if (sigsetsize != sizeof(sigset_t)) return -EINVAL;`.
        // Without this the engine ignored a2/a1 entirely and went to sleep on a call Linux rejects
        // instantly -- rt_sigsuspend(&set, 4) HUNG the guest forever instead of returning EINVAL.
        if (a1 != 8) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        uint64_t newmask;
        if (a0 == 0 || guest_copy_from(&newmask, a0, 8) != 8) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        uint64_t oldmask = c->sigmask;
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
        // The kernel copies the sigset and (if present) the timeout in from userspace up front, and writes
        // the dequeued siginfo out on success -- a bad pointer to any of the three is -EFAULT, never a fault
        // in the caller. Guard the direct derefs below to match (guest_bad_ptr also catches a PROT_NONE guard
        // page, as sigaltstack/rt_sigqueueinfo do). A NULL set is not a valid mask (kernel copy_from_user).
        //
        // sigsetsize (a3) is checked FIRST, before any pointer is touched: Linux's
        // SYSCALL_DEFINE4(rt_sigtimedwait) starts with `if (sigsetsize != sizeof(sigset_t)) return -EINVAL;`.
        // The engine ignored it, so rt_sigtimedwait(&set, NULL, NULL, 4) -- which Linux rejects
        // immediately -- instead entered the untimed poll loop and HUNG the guest forever.
        if (a3 != 8) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
        uint64_t set = 0;
        struct timespec timeout_copy;
        if (guest_copy_from(&set, a0, 8) != 8 ||
            (a1 && guest_accessible_prefix(a1, 128, HL_LOGICAL_VMA_WRITE) != 128) ||
            (a2 && guest_copy_from(&timeout_copy, a2, 16) != 16)) {
            G_RET(c) = (uint64_t)(-EFAULT);
            break;
        }
        struct timespec *to = a2 ? &timeout_copy : NULL;
        const hl_host_services *host = effective_host_services();
        uint64_t deadline = UINT64_MAX;
        uint64_t fallback_waited = 0;
        uint64_t budget_ns = 0;
        int finite = to != NULL;
        int deadline_valid = 0;
        if (to && (to->tv_sec < 0 || to->tv_nsec < 0 || to->tv_nsec >= 1000000000L)) {
            G_RET(c) = (uint64_t)(-EINVAL);
            break;
        }
        if (to) {
            uint64_t seconds = (uint64_t)to->tv_sec;
            budget_ns = seconds > (UINT64_MAX - (uint64_t)to->tv_nsec) / UINT64_C(1000000000)
                            ? UINT64_MAX
                            : seconds * UINT64_C(1000000000) + (uint64_t)to->tv_nsec;
            if (budget_ns != 0 && host && host->clock && host->clock->monotonic_ns) {
                hl_host_result now = host->clock->monotonic_ns(host->context);
                if (now.status == HL_STATUS_OK) {
                    deadline = now.value > UINT64_MAX - budget_ns ? UINT64_MAX : now.value + budget_ns;
                    deadline_valid = 1;
                }
            }
        }
        // An awaited signal with NO guest handler is invisible to g_pending unless a host handler catches it:
        // a cross-process (or host) kill(2) would otherwise hit the default disposition and terminate us
        // instead of being consumed synchronously here (this is what made a plain sigwait() fail). Install the
        // engine's SA_SIGINFO host handler on each such awaited signal's macOS number so it becomes pending,
        // then restore the prior disposition after the wait. (Signals that already have a guest handler route
        // to g_pending via that handler's host_sigh_si; unmaskable/synchronous signals are skipped.)
        struct sigaction saved[65];
        uint64_t installed = 0;
        for (int s = 1; s <= 64; s++) {
            if (!(set & (1ull << (s - 1))) || s == 9 || s == 19 || sig_is_sync(s) ||
                sig_host_is_engine_control(sig_l2m(s)))
                continue;
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
                // Synchronously dequeue ONE instance. The ascending scan picks the lowest signo (sigwaitinfo
                // priority order); sigq_pop takes the oldest queued instance of that signo (FIFO within a
                // signo) and carries its si_value/pid/code -- a realtime signal keeps its pending bit while
                // more instances remain. If nothing was queued (host async path), fall back to the
                // single-slot g_sig* the host handler wrote and clear the bit.
                struct sigq_ent ent;
                int popped = sigq_pop(got, &ent);
                __atomic_and_fetch(&c->tpending, ~(1ull << got), __ATOMIC_SEQ_CST);
                if (!popped) __atomic_and_fetch(&g_pending, ~(1ull << got), __ATOMIC_SEQ_CST);
                if (a1) { // fill siginfo_t whenever info != NULL (a3 is the sigsetsize, not a size threshold)
                    unsigned char result_info[128] = {0};
                    int error = popped ? ent.error : g_sigerror[got];
                    int code = popped ? ent.code : g_sigcode[got];
                    memcpy(result_info, &got, sizeof got);
                    memcpy(result_info + 4, &error, sizeof error);
                    memcpy(result_info + 8, &code, sizeof code);
                    int spid = popped ? ent.pid : g_sigpid[got];
                    if (spid) {
                        int suid = popped ? ent.uid : g_siguid[got];
                        memcpy(result_info + 16, &spid, sizeof spid);
                        memcpy(result_info + 20, &suid, sizeof suid);
                    }
                    uint64_t value = popped ? ent.value : g_sigval[got];
                    memcpy(result_info + 24, &value, sizeof value);
                    if (guest_copy_to(a1, result_info, sizeof result_info) != (ssize_t)sizeof result_info) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        break;
                    }
                    if (!popped) {
                        g_sigerror[got] = 0;
                        g_sigcode[got] = 0;
                        g_sigval[got] = 0;
                        g_sigpid[got] = 0;
                        g_siguid[got] = 0;
                    }
                }
                G_RET(c) = (uint64_t)got;
                break;
            }
            if ((finite && budget_ns == 0) || (finite && fallback_waited >= budget_ns) ||
                __atomic_load_n(&c->exited, __ATOMIC_SEQ_CST)) {
                G_RET(c) = (uint64_t)(-HL_LINUX_EAGAIN);
                break;
            }
            uint64_t interval = UINT64_C(2000000);
            if (finite && !deadline_valid && interval > budget_ns - fallback_waited)
                interval = budget_ns - fallback_waited;
            if (host && host->clock && host->clock->monotonic_ns && host->clock->sleep_until) {
                hl_host_result now = host->clock->monotonic_ns(host->context);
                if (now.status == HL_STATUS_OK) {
                    uint64_t slice_deadline = now.value > UINT64_MAX - interval ? UINT64_MAX : now.value + interval;
                    if (finite && deadline_valid) {
                        if (now.value >= deadline) {
                            G_RET(c) = (uint64_t)(-HL_LINUX_EAGAIN);
                            break;
                        }
                        if (slice_deadline > deadline) slice_deadline = deadline;
                    }
                    hl_host_result slept =
                        host->clock->sleep_until(host->context, HL_HOST_CLOCK_MONOTONIC, slice_deadline);
                    if (slept.status == HL_STATUS_OK || slept.status == HL_STATUS_INTERRUPTED) {
                        if (finite && !deadline_valid) fallback_waited += interval;
                        continue;
                    }
                }
            }
            if (finite && interval > budget_ns - fallback_waited) interval = budget_ns - fallback_waited;
            if (host && host->clock && host->clock->backoff_ns) (void)host->clock->backoff_ns(host->context, interval);
            if (finite) fallback_waited += interval;
        }
        ts_wait_leave();
        for (int s = 1; s <= 64; s++)
            if (installed & (1ull << s)) sigaction(sig_l2m(s), &saved[s], NULL); // restore disposition
        break;
    }
    // rt_sigaction(sig, *act, *old)
    case 134: {
        int sig = (int)a0;
        // Linux validates the ABI sigsetsize (a3) up front: it must equal sizeof(kernel sigset)=8, exactly
        // as rt_sigprocmask (case 135) does. The kernel checks this before copy_from_user of act, so a
        // wrong size is -EINVAL regardless of sig/act/oldact.
        if ((size_t)a3 != 8) {
            G_RET(c) = (uint64_t)(int64_t)(-EINVAL);
            break;
        }
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
        // glibc's kernel_sigaction on BOTH aarch64 and x86_64 carries an sa_restorer slot between sa_flags
        // and sa_mask, so the ABI struct is 32 bytes with the mask at offset 24. (The aarch64 leg previously
        // used 24/16, which read a zero where sa_mask actually sits -- so a handler's sa_mask members were
        // dropped from the in-handler signal mask; mask_in_handler.) x86 additionally consults the restorer
        // trampoline pointer (guarded by HL_GUEST_SIGACTION_HAS_RESTORER below).
        const uint64_t action_size = 32, mask_offset = 24;
        // The act/oldact structs are read/written DIRECTLY by the engine, so
        // a bad/unmapped pointer must return -EFAULT rather than fault the engine. Validate in Linux
        // order -- copyin `act` (a1) before copyout `oldact` (a2) -- so no oldact is written when act faults.
        uint64_t incoming[4] = {0}, outgoing[4] = {0};
        if (a1 && guest_copy_from(incoming, a1, (size_t)action_size) != (ssize_t)action_size) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a2 && guest_accessible_prefix(a2, (size_t)action_size, HL_LOGICAL_VMA_WRITE) != action_size) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a2) {
            outgoing[0] = g_sigact[sig].handler;
            outgoing[1] = g_sigact[sig].flags;
#if defined(HL_GUEST_SIGACTION_HAS_RESTORER)
            outgoing[2] = g_sigact[sig].restorer;
#endif
            outgoing[mask_offset / 8] = g_sigact[sig].mask;
            if (guest_copy_to(a2, outgoing, (size_t)action_size) != (ssize_t)action_size) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                break;
            }
        }
        if (a1) {
            uint64_t h = incoming[0];
            g_sigact[sig].handler = h;
            g_sigact[sig].flags = incoming[1];
#if defined(HL_GUEST_SIGACTION_HAS_RESTORER)
            g_sigact[sig].restorer = incoming[2];
#else
            g_sigact[sig].restorer = 0;
#endif
            g_sigact[sig].mask = incoming[mask_offset / 8];
            // Setting SIG_IGN (or SIG_DFL on a default-ignore signal) DISCARDS any pending instance --
            // Linux flushes the pending queue on an ignore transition. Without this a signal raised while
            // blocked, then set to SIG_IGN, stayed pending (ignore_discards_pending: sigpending must clear).
            if (h == 1 || (h == 0 && !sig_default_terminates(sig))) {
                sigq_flush(sig);
                __atomic_and_fetch(&c->tpending, ~(1ull << sig), __ATOMIC_SEQ_CST);
            }
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
                // STW_SIG and THREAD_INT_SIG are engine control channels on
                // native Linux. Guest dispositions remain virtual in
                // g_sigact; never replace their host handlers.
                if (sig_host_is_engine_control(ms)) {
                    // no host disposition change
                } else if (h == 0 && sig == 17 && (g_sigact[sig].flags & 0x2)) {
                    // SA_NOCLDWAIT with the DEFAULT SIGCHLD disposition still auto-reaps children (Linux
                    // leaves no zombie -> a later wait() gives ECHILD). SIG_DFL for SIGCHLD would install no
                    // host handler, so the auto-reap in host_sigh_si never ran and zombies lingered. Install
                    // our SIGCHLD handler so the reap path runs; the guest disposition is still SIG_DFL
                    // (h==0) so maybe_deliver_signal takes SIGCHLD's ignore default -- no guest handler fires.
                    struct sigaction sa;
                    memset(&sa, 0, sizeof sa);
                    sa.sa_sigaction = host_sigh_si;
                    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
                    sigfillset(&sa.sa_mask);
                    sigaction(ms, &sa, NULL);
                } else if (h == 0)
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
                    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
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
                    sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
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
        // The kernel copy_from_user()s `set` and copy_to_user()s `old`, so a bad/unmapped `set` or `old`
        // pointer is -EFAULT (like rt_sigpending case 136), not an engine fault writing/reading the mask.
        // guest_bad_ptr (not host_range_mapped) so a PROT_NONE guard page is caught too. Validate both
        // before mutating the mask so no partial state is left behind.
        uint64_t set = 0;
        if (a1 && guest_copy_from(&set, a1, 8) != 8) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a2 && guest_accessible_prefix(a2, 8, HL_LOGICAL_VMA_WRITE) != 8) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a2 && guest_copy_to(a2, &c->sigmask, 8) != 8) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        if (a1) {
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
        if (a1 && (a0 == 2 || (set & STOPBITS))) {
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
        if (guest_accessible_prefix(a0, 8, HL_LOGICAL_VMA_WRITE) != 8) {
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
        if (guest_copy_to(a0, &out, 8) != 8) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            break;
        }
        G_RET(c) = 0;
        break;
    }
    case 139:
        sigreturn_frame(c); // do_sigreturn + the non-PIE frame fold (../signal.c)
        c->redirect = 1;
        // rt_sigreturn (restorer path). The handler-return defer-pop + serial next-signal delivery happen in
        // the shared dispatcher's SIGRETURN_PC path (core/dispatch.c), which is where glibc's restorer lands.
        break;
    default: return 0;
    }
#if defined(HL_GUEST_SIGACTION_HAS_RESTORER)
    /* x86 self-signal syscalls can resume inside their translated block. At
       this point their architectural return value has been committed, so an
       unblocked pending signal can build its frame with the correct saved
       RAX and SA_NODEFER recursion nests before the handler continues. */
    if (nr != 139) maybe_deliver_signal(c);
#endif
    return 1;
}
