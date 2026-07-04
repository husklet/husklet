// dd/runtime/os/linux -- signal delivery (Linux<->macOS signal-number translation; sigframe build).

// #238 ptrace signal-delivery/group stops. Defined later in the TU (os/linux/syscall/ptrace.c, pulled in
// via dispatch.c). ptrace_intercept_signal: this process is traced and a signal is about to be delivered
// -> enter a signal/group ptrace-stop, report it to the tracer, block until CONT/SYSCALL, and (when the
// tracer re-injects it) resume with the effective signal in *out_sig; returns 1 iff it consumed `sig`
// (the caller must NOT deliver it) or 0 to deliver normally. Both are inert (fast 0) for an untraced
// process, which is the entire test matrix.
struct cpu;
static int ptrace_intercept_signal(struct cpu *c, int sig, int *out_sig);

// ---------------- signals ----------------
// Handlers are process-wide; the blocked mask is per-thread (cpu->sigmask).
// Async signals set a pending bit from a tiny host handler; the dispatcher then
// builds a Linux rt_sigframe on the guest stack and redirects to the handler.
static struct {
    uint64_t handler, flags, mask;
} g_sigact[65];
// bitmask of pending signals (1<<signo)
static volatile uint64_t g_pending;
// Per-thread "this guest thread is currently inside a host syscall on the guest's behalf" flag, set
// around service() (os/linux/syscall/dispatch.c). It is the reliable discriminator, when a fault-class
// signal (SIGSEGV/BUS/ILL/TRAP/FPE) arrives via a POSIX handler with the host PC NOT in translated code,
// between (a) an EXTERNAL kill(2)/tgkill of that signal at a thread blocked in a syscall -- which Linux
// delivers as an ordinary async signal that must wake pause()/sigsuspend()/read()/... (LTP pause01) --
// and (b) a genuine engine-code fault. macOS gives no usable siginfo discriminator for these (a
// kill-delivered SIGSEGV/ILL/FPE carries si_pid==0 and an si_addr just like a hardware fault), so we key
// off this instead: in a syscall => async guest delivery; otherwise => the real fault path / re-raise.
static __thread int g_in_service;
// rt_sigqueueinfo extras carried to the handler's siginfo: si_code + si_value (consumed on delivery)
static int g_sigcode[65];
static uint64_t g_sigval[65];
// SA_SIGINFO sender identity (si_pid/si_uid) captured from the host siginfo for a kill(2)/tgkill-delivered
// signal (consumed on delivery). g_sigpid==0 means "no sender identity" (async fault/internal), so a kill
// stamp is distinguishable from the sigfault si_addr that shares the same union offset.
static int g_sigpid[65];
static int g_siguid[65];
// synchronous-fault address carried to the handler's siginfo (si_addr; consumed on delivery, 0 for async)
static uint64_t g_sigaddr[65];
// sentinel lr: handler return -> sigreturn
#define SIGRETURN_PC 0xFFFFFFFFFFF0ull

static int sig_is_sync(int s) {
    return s == 4 || s == 5 || s == 7 || s == 8 || s == 11;
// ILL TRAP BUS FPE SEGV (Linux nums)
}
// Does signal `sig`'s DEFAULT action terminate the process (Term or Core)? False for the signals whose
// default action is ignore (CHLD/CONT/URG/WINCH) or stop (STOP/TSTP/TTIN/TTOU); true for every other
// deliverable signal (HUP/INT/QUIT/TERM/USRn/PIPE/ALRM/SEGV/... and the realtime signals 32..64).
static int sig_default_terminates(int sig) {
    switch (sig) {
    case 17: // SIGCHLD  -- ignore
    case 18: // SIGCONT  -- continue (no-op on delivery)
    case 23: // SIGURG   -- ignore
    case 28: // SIGWINCH -- ignore
    case 19: // SIGSTOP  -- stop
    case 20: // SIGTSTP  -- stop
    case 21: // SIGTTIN  -- stop
    case 22: // SIGTTOU  -- stop
        return 0;
    default: return sig >= 1 && sig <= 64;
    }
}
// Signal numbers diverge: Linux SIGUSR1=10/CHLD=17/BUS=7/SYS=31/USR2=12/URG=23/IO=29/STOP=19/
// CONT=18/TSTP=20 vs macOS 30/20/10/12/31/16/23/17/19/18. Translate at the host boundary.
static int sig_l2m(int s) {
    static const unsigned char T[32] = {0,  1,  2,  3,  4,  5,  6,  10, 8,  9,  30, 11, 31, 13, 14, 15,
                                        16, 20, 19, 17, 18, 21, 22, 16, 24, 25, 26, 27, 28, 23, 30, 12};
    return (s >= 1 && s <= 31) ? T[s] : s;
}
static int sig_m2l(int s) {
    static const unsigned char T[32] = {0,  1,  2,  3,  4,  5,  6,  7,  8,  9,  7,  11, 31, 13, 14, 15,
                                        23, 19, 20, 18, 17, 21, 22, 29, 24, 25, 26, 27, 28, 29, 10, 12};
    return (s >= 1 && s <= 31) ? T[s] : s;
}
// signalfd self-pipe (write end poked from host_sigh)
static int g_sigfd_pipe[2] = {-1, -1};
// its read end (the guest's signalfd)
static int g_sigfd_read = -1;
// signals routed to the signalfd (1<<signo)
static volatile uint64_t g_sigfd_mask;
// Shared body: mark Linux signal `ls` pending, kick the running thread out of any in-cache loop (#292),
// and wake a signalfd routed to it.
static void host_sig_pend(int ls) {
    __atomic_or_fetch(&g_pending, 1ull << ls, __ATOMIC_SEQ_CST);
    // #292: kick this thread out of any no-syscall in-cache loop so the caught signal is delivered at the
    // next block boundary (the emitted body check polls cpu->irq). This runs on the thread the OS picked,
    // which for a process-directed signal to a busy single-threaded guest IS the spinner.
    // pthread_getspecific is a plain TLS read; the store is a single aligned word.
    struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
    if (c) __atomic_store_n(&c->irq, 1, __ATOMIC_SEQ_CST);
    if ((g_sigfd_mask & (1ull << ls)) && g_sigfd_pipe[1] >= 0) {
        char b = (char)ls;
        if (write(g_sigfd_pipe[1], &b, 1) < 0) {}
    // wake signalfd/epoll
    }
}
static void host_sigh(int sig) { host_sig_pend(sig_m2l(sig)); } // host(macOS) signo -> Linux
// SA_SIGINFO host handler: same delivery as host_sigh, plus it captures the sender's pid/uid so an
// SA_SIGINFO guest handler (or sigwaitinfo) sees si_pid/si_uid. macOS populates si_pid for a kill(2) but
// does NOT set the Linux SI_USER si_code, so gate on si_pid>0 (a real sender) rather than the code.
static void host_sigh_si(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    int ls = sig_m2l(sig);
    if (si && si->si_pid > 0) {
        g_sigpid[ls] = (int)si->si_pid;
        g_siguid[ls] = (int)si->si_uid;
    }
    host_sig_pend(ls);
}
// Host handler for the NON-guarded synchronous-fault signals (SIGILL/SIGTRAP/SIGFPE) when the guest
// installs a handler for them. A REAL hardware fault for these never reaches a POSIX handler on this
// platform -- arm64 delivers an illegal instruction via the Mach exception port (-> deliver_guest_fault)
// and x86 integer #DE is synthesized at the dispatcher -- so anything arriving HERE is an EXTERNAL
// kill(2)/tgkill/sigqueue (LTP pause01 kills a paused process with SIGILL/SIGTRAP/SIGFPE). Linux
// delivers those as ordinary async signals that wake pause()/sigsuspend() and run the handler: while the
// thread is in a syscall (g_in_service) mark it pending (the async path host_sigh_si uses) and capture
// the sender. Otherwise (a genuine fault that somehow surfaced as POSIX) restore the default and re-raise.
static void host_sigh_sync(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    int ls = sig_m2l(sig);
    // Only ever an EXTERNAL kill(2) when it lands while the thread is blocked in a syscall (g_in_service):
    // a real illegal-instruction/#DE never reaches a POSIX handler here (arm64 uses the Mach exception
    // port, x86 synthesizes #DE at the dispatcher). In-syscall => deliver async (wakes pause()/sigsuspend
    // + runs the handler); otherwise a genuine fault surfaced as POSIX -> restore default and re-raise.
    if (!g_in_service) {
        signal(sig, SIG_DFL);
        raise(sig);
        return;
    }
    if (si && si->si_pid > 0) { g_sigpid[ls] = (int)si->si_pid; g_siguid[ls] = (int)si->si_uid; }
    host_sig_pend(ls);
}

// build_signal_frame + do_sigreturn are per-arch (the sigframe register layout) -> frontend/<arch>/sigframe.c
static void build_signal_frame(struct cpu *c, int sig);
static void do_sigreturn(struct cpu *c);
// per-arch (the host<->guest register model differs): on a synchronous fault inside translated code,
// reconstruct the guest register state from the host fault context (returns 1 iff the faulting host PC is
// in the code cache), and steer the host context back into the dispatcher so a guest handler can run.
static int sigframe_capture_fault(struct cpu *c, void *ucv);
static void sigframe_resume_dispatch(struct cpu *c, void *ucv);
static void maybe_deliver_signal(struct cpu *c) {
    // Two sources: g_pending (process-directed -- any thread may take it) and c->tpending (thread-directed
    // via tkill/tgkill -- only THIS thread). Consider both; coalescing a process- and thread-directed
    // instance of the same (non-realtime) signal into one delivery is the correct Linux semantics.
    uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
    for (int sig = 1; sig <= 64; sig++) {
        uint64_t bit = 1ull << sig;
        // sigmask is sigset_t (bit N-1)
        if (!(p & bit) || (c->sigmask & (1ull << (sig - 1)))) continue;
        uint64_t h = g_sigact[sig].handler;
        if (h <= 1) {
            // No guest handler -- clear this pending instance from both queues.
            __atomic_and_fetch(&g_pending, ~bit, __ATOMIC_SEQ_CST);
            __atomic_and_fetch(&c->tpending, ~bit, __ATOMIC_SEQ_CST);
            // A SIG_DFL signal whose default action TERMINATES, still pending at the container init, was NOT
            // already actioned by the host: real Linux protects a PID-namespace init from an unhandled fatal
            // signal, so it lingered (e.g. the guest blocked it inside its handler, reset the disposition to
            // SIG_DFL, then re-raised it to exit -- exactly node's SignalExit / mongosh path). dd's init is
            // just the container entrypoint, not an init that must survive, so take the default action and end
            // the container with 128+signo (the code `docker run` reports for a PID 1 killed by a signal).
            // SIG_IGN (h==1) and the default-ignore/stop signals stay dropped here.
            if (h == 0 && container_pid() == 1 && sig_default_terminates(sig)) {
                c->exited = 1;
                c->exit_code = 128 + sig;
                return;
            }
            continue;
        }
        // Claim from both queues (clear unconditionally so the coalesced signal is delivered exactly once),
        // then run the guest handler on this thread.
        uint64_t had_t = __atomic_fetch_and(&c->tpending, ~bit, __ATOMIC_SEQ_CST) & bit;
        uint64_t had_p = __atomic_fetch_and(&g_pending, ~bit, __ATOMIC_SEQ_CST) & bit;
        if (had_t || had_p) {
            build_signal_frame(c, sig);
            return;
        }
    }
}
// A signal aimed at our own process (raise/abort/pthread_kill). Deliver it through our
// own machinery instead of a real host signal (host signals into a MAP_JIT thread are
// fragile): a guest handler -> pending bit; otherwise apply the default action here.
static void raise_guest_signal(struct cpu *c, int sig) {
    if (sig < 1 || sig > 64) return;
    // #238: if this process is traced, a signal it raises on itself (raise/abort/kill-self, incl. the
    // raise(SIGSTOP) tracers' children use) becomes a ptrace signal/group-stop reported to the tracer.
    // The tracer then decides the effective signal to deliver (0 = suppress). Inert for an untraced
    // process (fast 0), which is the entire test matrix.
    {
        int eff = sig;
        if (ptrace_intercept_signal(c, sig, &eff)) {
            if (eff <= 0) return; // suppressed by the tracer
            sig = eff;            // deliver the tracer's (possibly changed) signal below, no re-trap
        }
    }
    uint64_t h = g_sigact[sig].handler;
    if (h > 1) {
        __atomic_or_fetch(&g_pending, 1ull << sig, __ATOMIC_SEQ_CST);
        return;
    // custom handler
    }
    // SIG_IGN
    if (h == 1) return;
    // blocked: make pending (signalfd / deliver on unblock)
    if (c && (c->sigmask & (1ull << (sig - 1)))) {
        __atomic_or_fetch(&g_pending, 1ull << sig, __ATOMIC_SEQ_CST);
        if ((g_sigfd_mask & (1ull << sig)) && g_sigfd_pipe[1] >= 0) {
            char b = (char)sig;
            if (write(g_sigfd_pipe[1], &b, 1) < 0) {}
        }
        return;
    }
    // SIGCHLD/CONT/URG/WINCH: ignore
    if (sig == 17 || sig == 18 || sig == 23 || sig == 28) return;
    // Unhandled fatal signal aimed at the container init: real Linux would protect a PID-namespace init and
    // drop it, but dd's init is just the entrypoint -- take the default action and end the container with
    // 128+signo (what `docker run` reports for a PID 1 killed by a signal) rather than raising a real host
    // signal that kills the engine BY the signal. The stop signals keep the host path below (job control
    // mirrors them onto the host mask, so a real host stop is the correct default action).
    if (container_pid() == 1 && sig_default_terminates(sig)) {
        c->exited = 1;
        c->exit_code = 128 + sig;
        return;
    }
    signal(sig_l2m(sig), SIG_DFL);
    // default: a non-init guest process IS the engine process -- a real host signal both terminates it and
    // yields the correct WIFSIGNALED status to its parent's waitpid (host signo).
    raise(sig_l2m(sig));
    c->exited = 1;
    // fallback if raise returns / signo invalid on host
    c->exit_code = 128 + sig;
}

// A synchronous CPU fault (SIGSEGV/SIGBUS) taken inside translated code is the GUEST's own fault. If the
// guest installed a handler for it, reconstruct the guest register state from the host fault context,
// synthesize the Linux siginfo (si_addr = the guest fault address), queue the signal, and steer the host
// context back into the dispatcher so the handler runs and sigreturn/siglongjmp resumes. Called from the
// per-arch SIGSEGV/SIGBUS guard AFTER its own engine-managed fixups (non-PIE data-ref / SMC / lazy map)
// decline. `hostsig` is the macOS signo; returns 1 iff the fault was routed to a guest handler.
//
// We deliberately do NOT build the guest sigframe here: this host handler runs on the faulting thread's
// stack, which on the aarch64 frontend IS the guest stack (the block's host SP == guest SP), so writing the
// frame inline would clobber the live handler stack. Instead we mark the signal pending and hand control
// back to run_guest -- its maybe_deliver_signal builds the frame in the engine's own stack context (the
// exact, already-tested async-delivery path). A synchronous fault cannot be ignored or masked, so force it
// deliverable first.
static int deliver_guest_fault(int hostsig, siginfo_t *si, void *ucv) {
    int sig = sig_m2l(hostsig);
    if (sig < 1 || sig > 64 || !ucv) return 0;
    // SIG_DFL/SIG_IGN: not the guest's to handle -> let the guard re-raise (a real crash).
    if (g_sigact[sig].handler <= 1) return 0;
    struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
    if (!c) return 0;
    if (!sigframe_capture_fault(c, ucv)) {
        // The faulting host PC is NOT inside translated code, so this is not the guest's own CPU fault.
        // Two cases: (a) an EXTERNAL process kill(2)/tgkill'd this fault-class signal at us while the
        // thread was blocked in a syscall -- e.g. LTP pause01 sends SIGSEGV to a process in pause() --
        // which Linux delivers as an ordinary ASYNC signal that wakes the blocked call (EINTR) and runs
        // the handler; or (b) a genuine engine fault in our own C code. macOS gives no usable siginfo
        // discriminator (a kill'd SIGSEGV carries si_pid==0 + an si_addr like a hardware fault), so key
        // off g_in_service: inside a syscall => (a). Queue it pending + kick any in-cache loop out (irq)
        // and report handled -- the host guard returns, the blocking call wakes, and maybe_deliver_signal
        // builds the frame at the next dispatch (leave si_code/si_addr 0 == SI_USER, the kill siginfo).
        // Not in a syscall => (b), not ours: re-raise.
        if (g_in_service) {
            if (si && si->si_pid > 0) { g_sigpid[sig] = (int)si->si_pid; g_siguid[sig] = (int)si->si_uid; }
            __atomic_or_fetch(&g_pending, 1ull << sig, __ATOMIC_SEQ_CST);
            __atomic_store_n(&c->irq, 1, __ATOMIC_SEQ_CST);
            return 1;
        }
        return 0;
    }
    g_sigaddr[sig] = si ? (uint64_t)si->si_addr : 0;
    // Linux si_code for a hardware fault: SIGBUS -> BUS_ADRERR(2), else SEGV_MAPERR(1).
    g_sigcode[sig] = (sig == 7) ? 2 : 1;
    c->sigmask &= ~(1ull << (sig - 1)); // a sync fault forces delivery even if the guest blocked it
    c->reason = R_BRANCH;               // resume as a plain branch (no stale syscall/special-op handling)
    __atomic_or_fetch(&g_pending, 1ull << sig, __ATOMIC_SEQ_CST);
    sigframe_resume_dispatch(c, ucv);
    return 1;
}

// Linux mmap flags -> macOS.
static int mmap_flags(int lf) {
    int f = 0;
    if (lf & 0x01) f |= MAP_SHARED;
    if (lf & 0x02) f |= MAP_PRIVATE;
    if (lf & 0x10) f |= MAP_FIXED;
    if (lf & 0x20) f |= MAP_ANON;
    return f;
}
