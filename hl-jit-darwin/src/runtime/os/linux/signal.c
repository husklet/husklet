// dd/runtime/os/linux -- signal delivery (Linux<->macOS signal-number translation; sigframe build).

// ptrace signal-delivery/group stops. Defined later in the TU (os/linux/syscall/ptrace.c, pulled in
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

// ---------------- Go async-preempt SIGURG suppression for iscgo aarch64 images (#423) ----------------
// Go's scheduler asynchronously preempts a running goroutine by sending itself SIGURG (23) and injecting a
// call to runtime.asyncPreempt from the signal handler. For an EXTERNALLY-LINKED / cgo (runtime.iscgo==1)
// aarch64 Go binary, delivering SIGURG into the guest crashes it -- but NOT via a sigframe/SP overlap (that
// hypothesis was investigated with the go_cgo_stackgrow_arm fixture under DD_SIGURG=deliver + CRASHDBG and
// DISPROVEN: build_signal_frame captures a correct, consistent guest SP/PC/LR and the frame sits strictly
// below SP). The real defect is ASYNC-PREEMPT SAFETY: dd delivers the caught signal at TRANSLATION-BLOCK
// boundaries (the cpu->irq poll), which do NOT coincide with Go's compiler-inserted async-safe points. Go's
// own guard (doSigPreempt -> isAsyncSafePoint / canPreemptM) is meant to no-op an unsafe delivery, but under
// dd's non-PIE HIGH-bias translation the interaction with Go's stack-growth machinery corrupts state: the
// fixture's crash lands squarely in runtime.copystack / runtime.adjustframe / (*stkframe).getStackMap with
// `fatal error: wirep: already in go` and `untyped locals` (a stack-map/scheduler invariant violation), i.e.
// an async preempt injected around a stack copy rewrites a frame's return address / pointer. A correct fix
// requires honoring Go's async-safe-point model (parsing the guest pclntab's PCDATA_UnsafePoint, or a
// safepoint-accurate delivery scheme) -- a large, fragile undertaking not yet landed. Until then we suppress
// SIGURG for exactly this class, which is functionally identical to Go's OWN supported `GODEBUG=
// asyncpreemptoff=1`: async (tight-loop) preemption is disabled, but COOPERATIVE preemption at safepoints
// still works, so the program runs correctly (proven: this fixture completes, influxd boots, vmetrics serves).
//
// Scoped TIGHTLY on purpose: g_go_iscgo (set once by the aarch64 load_elf in os/linux/elf.c) is 1 ONLY for a
// cgo-enabled aarch64 Go main image. It stays 0 for non-Go guests (some legitimately use SIGURG for OOB TCP
// data), for internal-linked / CGO_ENABLED=0 Go, and for the entire x86 engine (that TU never includes the
// aarch64 elf.c, and no x86 path sets it). Env override DD_SIGURG forces it: `drop`/`off` = always suppress,
// `deliver`/`async`/`on` = always deliver (disable the mitigation); legacy DDDBG_DROPURG=1 = force suppress.
int g_go_iscgo; // 1 iff the loaded aarch64 main image is a cgo (iscgo) Go binary; owned here, set by load_elf

// Should SIGURG (Go async-preempt) delivery be dropped for this process? Env override wins; else auto on the
// detected iscgo class. Env is parsed once (it never changes); g_go_iscgo is fixed before any signal fires.
static int sigurg_drop_enabled(void) {
    static int cached = -2; // -2 uncomputed; -1 auto; 0 force-deliver; 1 force-drop
    if (cached == -2) {
        const char *e = getenv("DD_SIGURG");
        if (e && (!strcmp(e, "deliver") || !strcmp(e, "async") || !strcmp(e, "on")))
            cached = 0;
        else if (e && (!strcmp(e, "drop") || !strcmp(e, "off")))
            cached = 1;
        else if (getenv("DDDBG_DROPURG"))
            cached = 1; // legacy repro/debug knob
        else
            cached = -1; // auto
    }
    if (cached >= 0) return cached;
    return g_go_iscgo ? 1 : 0;
}

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

// Does signal `sig`'s DEFAULT action produce a core dump (Linux "Core" disposition)? Exactly the set LTP
// waitpid01 expects: QUIT/ILL/TRAP/ABRT/BUS/FPE/SEGV/XCPU/XFSZ/SYS. Everything else that terminates does
// so as plain "Term" (no core). Used to set WCOREDUMP faithfully on a guest signal death.
static int sig_coredumps(int sig) {
    switch (sig) {
    case 3:  // SIGQUIT
    case 4:  // SIGILL
    case 5:  // SIGTRAP
    case 6:  // SIGABRT
    case 7:  // SIGBUS
    case 8:  // SIGFPE
    case 11: // SIGSEGV
    case 24: // SIGXCPU
    case 25: // SIGXFSZ
    case 31: // SIGSYS
        return 1;
    default: return 0;
    }
}

// Current soft RLIMIT_CORE (resource 4), guest-visible: a docker --ulimit / the guest's own
// setrlimit/prlimit64 store (g_ulimit, seeded in state.c) wins, else the dd default. A core dump only
// happens when a coredumping signal kills a process whose SOFT core limit is nonzero, so this is the single
// input WCOREDUMP is gated on. g_ulimit/DD_RLIM_MAX come from container/state.c (included first).
// The default MUST be 0 (cores OFF), matching getrlimit(RLIMIT_CORE)'s Linux/docker default soft=0 -- the
// old RLIM_INFINITY default contradicted getrlimit and made every crash report WCOREDUMP even though cores
// were disabled (wait4/waitid reported CLD_DUMPED while getrlimit said the core limit was 0).
static uint64_t svc_core_rlimit_cur(void) {
    if (4 < DD_RLIM_MAX && g_ulimit[4].set) return g_ulimit[4].cur;
    return 0; // Linux/docker default: cores OFF (soft RLIMIT_CORE = 0)
}

// ---------------- guest signal-death relay ----------------
// Every guest process is a real host (macOS) process, so a guest parent reaps its children with the host
// wait4/waitid and reads the host termination status. When a guest child must die from a fatal-default
// signal, dd normally lets it die BY the mapped host signal, and the parent's wait4 translates the host
// termsig back (sig_m2l). That fails for signals with NO faithful fatal host mapping: SIGPOLL(29)->host
// SIGIO / SIGSTKFLT(16)->host SIGURG both DEFAULT-IGNORE on macOS (so raising them does not terminate),
// and SIGPWR(30)->host SIGUSR1 maps BACK to a different signo (10). dd then fell back to _exit(128+signo),
// which the parent reads as WIFEXITED(128+signo) instead of WIFSIGNALED/WTERMSIG=signo.
//
// Fix: the dying child records its intended Linux termination signo (+ the WCOREDUMP verdict it computed
// from its own RLIMIT_CORE) into this MAP_SHARED table, keyed by its pid, then _exit()s. The reaping
// parent's wait4 (proc.c case 260) / waitid (rare.c case 95) looks the reaped pid up and reconstructs the
// exact Linux SIGNALED status. A NORMAL guest _exit(n) writes NOTHING here, so it is never misread as a
// signal death — that is the disambiguation guard between a real exit(128+n) and a signal termination.
//
// The page is created pre-fork (sigexit_init, called at every fork site) so every descendant inherits the
// same shared mapping. A slot is claimed by the dying child and cleared by the parent when it reaps the
// zombie, so a pid can never be reused while a stale entry survives (the zombie holds the pid until reap).
struct sigexit_ent {
    int pid; // 0 = free, -1 = claimed (mid-write), >0 = published guest pid; accessed via __atomic_* only
    int signo;
    int core;
};

#define SIGEXIT_SLOTS 4096
static struct sigexit_ent *g_sigexit;

static void sigexit_init(void) {
    if (g_sigexit) return;
    void *m =
        mmap(NULL, sizeof(struct sigexit_ent) * SIGEXIT_SLOTS, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANON, -1, 0);
    if (m != MAP_FAILED) g_sigexit = (struct sigexit_ent *)m;
}

// Dying child: publish (signo, core) for its own pid. Claim a free slot (0 -> -1), fill the payload, then
// store the real pid LAST so a concurrent parent scan never sees a half-written entry. Best-effort: if the
// table is full the child just _exit()s and the parent falls back to WIFEXITED(128+signo).
static void sigexit_record(int signo, int core) {
    if (!g_sigexit) return;
    int me = (int)getpid();
    for (int i = 0; i < SIGEXIT_SLOTS; i++) {
        int expect = 0;
        if (__atomic_compare_exchange_n(&g_sigexit[i].pid, &expect, -1, 0, __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) {
            g_sigexit[i].signo = signo;
            g_sigexit[i].core = core;
            __atomic_store_n(&g_sigexit[i].pid, me, __ATOMIC_RELEASE); // publish last
            return;
        }
    }
}

// Reaping parent: if `pid` recorded a guest signal death, set *signo/*core and return 1. `consume` clears
// the slot (a real reap); pass 0 for a WNOWAIT peek so a later real reap can still find it.
static int sigexit_lookup(int pid, int *signo, int *core, int consume) {
    if (!g_sigexit || pid <= 0) return 0;
    for (int i = 0; i < SIGEXIT_SLOTS; i++) {
        if (__atomic_load_n(&g_sigexit[i].pid, __ATOMIC_ACQUIRE) == pid) {
            *signo = g_sigexit[i].signo;
            *core = g_sigexit[i].core;
            if (consume) __atomic_store_n(&g_sigexit[i].pid, 0, __ATOMIC_RELEASE);
            return 1;
        }
    }
    return 0;
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

// sigsuspend/pause force-delivery mask (per-thread, bit = 1<<signo, same convention as g_pending). When
// rt_sigsuspend is interrupted by a signal it awaited, POSIX runs that handler DURING the suspend and then
// restores the pre-suspend mask on return -- so the handler must be delivered even when the restored mask
// BLOCKS it. Rather than clear the bit out of c->sigmask (which corrupted the mask the sigframe saves and
// restores -- LTP sigsuspend01), the syscall leaves c->sigmask at the correct post-suspend value and marks
// the awaited signal here; maybe_deliver_signal then delivers it once, ignoring the mask, and the sigframe
// saves/restores the true post-suspend mask. Cleared as the signal is claimed for delivery.
static __thread uint64_t g_force_deliver;

// --- signalfd open-file-description (OFD) pool ---------------------------------------------------------
// Linux signalfd(2) creates an INDEPENDENT descriptor: each has its own signal mask and its own delivery
// queue, so a SIGUSR1 signalfd and a SIGUSR2 signalfd never alias each other or broaden each other's masks.
// dd's old model was a SINGLE shared self-pipe with one ORed mask, which failed exactly that independence.
// Each signalfd OFD is now one slot here: a self-pipe (read end = the guest's signalfd, write end poked by
// the host signal handlers) + its own mask + a refcount (a dup(2) shares the OFD, so refs bumps and the
// pipe is torn down only when the last alias closes). The read end is a normal guest fd; only the write end
// is engine-private (relocated out of the guest's low fd range at create, protected from the guest's
// close/exec sweep). `g_sigfd_slot[fdnum]` maps a guest fd NUMBER to its OFD slot (+1); 0 = not a signalfd.
#define DD_SFD_MAX 64
struct sfd_ofd {
    int rd;                 // read end (a guest fd number)
    int wr;                 // write end (engine-private, poked on signal delivery)
    volatile uint64_t mask; // signals routed to THIS signalfd (1<<signo)
    int refs;               // fd aliases referring to this OFD (dup bumps); 0 = free slot
};
static struct sfd_ofd g_sfd[DD_SFD_MAX];
static uint8_t g_sigfd_slot[DD_NFD]; // guest fd number -> OFD slot index + 1 (0 = not a signalfd)

// Allocate a free OFD slot (refs==0). Returns the slot index or -1 if the pool is exhausted.
static int sfd_alloc(void) {
    for (int i = 0; i < DD_SFD_MAX; i++)
        if (g_sfd[i].refs == 0) {
            g_sfd[i].rd = g_sfd[i].wr = -1;
            g_sfd[i].mask = 0;
            g_sfd[i].refs = 1;
            return i;
        }
    return -1;
}

// Deliver Linux signal `ls` to EVERY signalfd whose mask includes it. Each write is one queued byte encoding
// the signo, so a realtime signal delivered N times reads back as N siginfo records on each matching fd.
static void sfd_deliver(int ls) {
    if (ls < 1 || ls > 63) return;
    uint64_t bit = 1ull << ls;
    for (int i = 0; i < DD_SFD_MAX; i++)
        if (g_sfd[i].refs > 0 && g_sfd[i].wr >= 0 && (g_sfd[i].mask & bit)) {
            char b = (char)ls;
            if (write(g_sfd[i].wr, &b, 1) < 0) {}
        }
}

// Is host fd `fd` a signalfd write end? (engine-private -- must survive the guest's close/exec sweep.)
static int sfd_wr_is(int fd) {
    if (fd < 0) return 0;
    for (int i = 0; i < DD_SFD_MAX; i++)
        if (g_sfd[i].refs > 0 && g_sfd[i].wr == fd) return 1;
    return 0;
}

// Shared body: mark Linux signal `ls` pending, kick the running thread out of any in-cache loop,
// and wake every signalfd routed to it.
static void host_sig_pend(int ls) {
    __atomic_or_fetch(&g_pending, 1ull << ls, __ATOMIC_SEQ_CST);
    // kick this thread out of any no-syscall in-cache loop so the caught signal is delivered at the
    // next block boundary (the emitted body check polls cpu->irq). This runs on the thread the OS picked,
    // which for a process-directed signal to a busy single-threaded guest IS the spinner.
    // pthread_getspecific is a plain TLS read; the store is a single aligned word.
    struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
    if (c) __atomic_store_n(&c->irq, 1, __ATOMIC_SEQ_CST);
    sfd_deliver(ls); // wake signalfd/epoll (per-OFD mask)
}

static void host_sigh(int sig) {
    host_sig_pend(sig_m2l(sig));
} // host(macOS) signo -> Linux

static void sig_diag_sync_reraise(int sig, int ls, siginfo_t *si, void *ucv);
static void sig_diag_raise_default(struct cpu *c, int sig);

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
    // SA_NOCLDWAIT on the guest's SIGCHLD: Linux still DELIVERS the SIGCHLD but leaves no zombie. macOS's own
    // SA_NOCLDWAIT would suppress the signal entirely, so we don't set it (see rt_sigaction) -- instead
    // auto-reap every terminated child here (WNOHANG, and no WUNTRACED so a stopped child is left alone). The
    // guest handler still runs (host_sig_pend below) and a later wait() sees ECHILD. Gated on the guest opt-in.
    if (ls == 17 && (g_sigact[17].flags & 0x2)) {
        int wst;
        while (waitpid(-1, &wst, WNOHANG) > 0) {
        }
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
    int ls = sig_m2l(sig);
    // Only ever an EXTERNAL kill(2) when it lands while the thread is blocked in a syscall (g_in_service):
    // a real illegal-instruction/#DE never reaches a POSIX handler here (arm64 uses the Mach exception
    // port, x86 synthesizes #DE at the dispatcher). In-syscall => deliver async (wakes pause()/sigsuspend
    // + runs the handler); otherwise a genuine fault surfaced as POSIX -> restore default and re-raise.
    if (!g_in_service) {
        sig_diag_sync_reraise(sig, ls, si, uc);
        signal(sig, SIG_DFL);
        raise(sig);
        return;
    }
    if (si && si->si_pid > 0) {
        g_sigpid[ls] = (int)si->si_pid;
        g_siguid[ls] = (int)si->si_uid;
    }
    host_sig_pend(ls);
}

// Mach exception delivery runs on a dedicated helper thread, so it cannot read the faulting thread's
// g_in_service TLS directly. CRASHDBG passes the faulting cpu via x28; use its syscall-stamped guest PC as
// the cross-thread equivalent of "the target was in service(c)" for async fault-class signals caught by the
// Mach port before POSIX host_sigh_sync can run.
static int mach_async_fault_signal(struct cpu *c, int hostsig, siginfo_t *si) {
    int sig = sig_m2l(hostsig);
    if (!c || sig < 1 || sig > 64) return 0;
    if (!sig_is_sync(sig)) return 0;
    if (!host_range_mapped((uintptr_t)c, sizeof *c)) return 0;
    uint64_t pc = G_PC(c);
    if (!host_range_mapped((uintptr_t)pc, 4)) return 0;
    if (!pc || *(uint32_t *)pc != 0xD4000001u) return 0; // aarch64 svc #0
    if (si && si->si_pid > 0) {
        g_sigpid[sig] = (int)si->si_pid;
        g_siguid[sig] = (int)si->si_uid;
    }
    __atomic_or_fetch(&g_pending, 1ull << sig, __ATOMIC_SEQ_CST);
    __atomic_store_n(&c->irq, 1, __ATOMIC_SEQ_CST);
    return 1;
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
        // sigmask is sigset_t (bit N-1). A signal blocked by the mask is normally not delivered -- UNLESS it
        // was force-marked by rt_sigsuspend/pause (POSIX: the awaited handler runs during the suspend even
        // though the restored mask blocks it). g_force_deliver overrides the mask for exactly that one bit.
        if (!(p & bit)) continue;
        // INTERIM: suppress Go's async-preempt SIGURG (23) for a cgo aarch64 Go image (see the note at the
        // top of this file). Drop the pending instance from both queues so it is never delivered to the guest
        // handler; cooperative preemption keeps the program correct. Scoped to exactly the iscgo class + env.
        if (sig == 23 && sigurg_drop_enabled()) {
            __atomic_and_fetch(&g_pending, ~bit, __ATOMIC_SEQ_CST);
            __atomic_and_fetch(&c->tpending, ~bit, __ATOMIC_SEQ_CST);
            continue;
        }
        if ((c->sigmask & (1ull << (sig - 1))) && !(g_force_deliver & bit)) continue;
        uint64_t h = g_sigact[sig].handler;
        if (h <= 1) {
            // No guest handler -- clear this pending instance from both queues (and any force mark).
            g_force_deliver &= ~bit;
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
            g_force_deliver &= ~bit; // consumed: the sigframe (built below) saves the true post-suspend mask
            uint64_t flags = g_sigact[sig].flags;
            build_signal_frame(c, sig); // captures g_sigact[sig].handler as the target PC -- must run first
            // SA_RESETHAND (SA_ONESHOT, 0x80000000): the disposition reverts to SIG_DFL after this single
            // delivery (the handler PC is already baked into the frame above). Reset both the recorded
            // disposition and the emulated host disposition, so a second occurrence takes the default action
            // (LTP-style signal()-with-caller-reset semantics; glibc's legacy signal() sets SA_RESETHAND).
            if (flags & 0x80000000ull) {
                g_sigact[sig].handler = 0;
                if (sig != 9 && sig != 19 && !sig_is_sync(sig)) signal(sig_l2m(sig), SIG_DFL);
            }
            return;
        }
    }
}

// A signal aimed at our own process (raise/abort/pthread_kill). Deliver it through our
// own machinery instead of a real host signal (host signals into a MAP_JIT thread are
// fragile): a guest handler -> pending bit; otherwise apply the default action here.
static void raise_guest_signal(struct cpu *c, int sig) {
    if (sig < 1 || sig > 64) return;
    // if this process is traced, a signal it raises on itself (raise/abort/kill-self, incl. the
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
        sfd_deliver(sig);
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
        sig_diag_raise_default(c, sig);
        c->exited = 1;
        c->exit_code = 128 + sig;
        return;
    }
    // Non-init guest process dying from a fatal-default signal. A guest process IS a real host process, so
    // its parent reaps it with the host wait4/waitid. Raising the mapped HOST signal cannot faithfully carry
    // the Linux signo for every signal: SIGPOLL(29)/SIGSTKFLT(16) map to host signals that DEFAULT-IGNORE on
    // macOS (raise() then returns without terminating) and SIGPWR(30) maps to a host signal that reports a
    // DIFFERENT signo back (10). So instead of raising, record the intended Linux termination signal in the
    // shared relay and _exit(128+signo); the parent's wait4/waitid reconstructs WIFSIGNALED/WTERMSIG=sig
    // (proc.c case 260 / rare.c case 95). WCOREDUMP per Linux rules: a coredumping signal with soft
    // RLIMIT_CORE > 0. If the relay slot table is exhausted the parent simply sees the WIFEXITED(128+signo)
    // fallback — the same graceful degradation as before this fix.
    if (sig_default_terminates(sig)) {
        sig_diag_raise_default(c, sig);
        int core = sig_coredumps(sig) && svc_core_rlimit_cur() > 0;
        sigexit_record(sig, core);
        c->exited = 1;
        c->exit_code = 128 + sig;
        return;
    }
    // Non-terminating default reaching here = a stop signal (STOP/TSTP/TTIN/TTOU): mirror it onto the host so
    // a real job-control stop happens (the host mask mirrors these too — see rt_sigprocmask).
    signal(sig_l2m(sig), SIG_DFL);
    raise(sig_l2m(sig));
    c->exited = 1;
    c->exit_code = 128 + sig; // fallback if raise returns / signo invalid on host
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
// `cpu_hint` names the FAULTING thread's cpu, for a caller that does NOT run on that thread. The POSIX
// guard (nonpie_guard) runs ON the faulting thread, so it passes NULL and we read the cpu from this
// thread's TLS. The CRASHDBG aarch64 Mach handler (mach_resolve_fault) runs on a DEDICATED exc_thread whose
// g_cpu_key TLS is NULL -- so it MUST pass the faulting thread's cpu explicitly (recovered from that
// thread's x28==CPUREG register), or every guest-handled fault (a gcc/cc1/JVM/Go SIGSEGV handler, glibc
// stack-overflow detection, ...) is wrongly declined here and reported as a spurious [MACH] crash instead
// of being delivered. The hint is only ever dereferenced after sigframe_capture_fault confirms the faulting
// host PC is inside the code cache, where x28==cpu holds by construction, so a stale hint is never used.
static int deliver_guest_fault_hint(struct cpu *cpu_hint, int hostsig, siginfo_t *si, void *ucv) {
    int sig = sig_m2l(hostsig);
    if (sig < 1 || sig > 64 || !ucv) return 0;
    // Linux reports a BAD-ADDRESS fault (unmapped page / PROT_NONE guard / a stack overflow into the
    // guard gap) as SIGSEGV, but macOS raises a PROT_NONE access as host SIGBUS (-> Linux SIGBUS(7)). Rewrite
    // it to SIGSEGV(11) when the address is a tracked guard (gna_hit) or is unmapped, so a guest's own SIGSEGV
    // handler (glibc stack-overflow detection, a JIT/VM's guard-page trap) catches it. A genuine SIGBUS on
    // MAPPED memory (misalignment, file mapping past EOF) is left as SIGBUS. Only host SIGBUS needs the check
    // (host SIGSEGV already maps to Linux SIGSEGV), so the common fault path pays no extra probe.
    if (hostsig == SIGBUS && si && si->si_addr &&
        (gna_hit((uint64_t)si->si_addr, 1) || !host_addr_mapped((uintptr_t)si->si_addr)))
        sig = 11;
    // SIG_DFL/SIG_IGN: not the guest's to handle -> let the guard re-raise (a real crash).
    if (g_sigact[sig].handler <= 1) return 0;
    struct cpu *c = cpu_hint ? cpu_hint : (struct cpu *)pthread_getspecific(g_cpu_key);
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
            if (si && si->si_pid > 0) {
                g_sigpid[sig] = (int)si->si_pid;
                g_siguid[sig] = (int)si->si_uid;
            }
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

// POSIX-guard entry: the faulting thread IS this thread, so its cpu is in TLS (cpu_hint == NULL).
static int deliver_guest_fault(int hostsig, siginfo_t *si, void *ucv) {
    return deliver_guest_fault_hint(NULL, hostsig, si, ucv);
}

static void sig_diag_hex16(char *p, uint64_t v) {
    static const char h[] = "0123456789abcdef";
    for (int i = 15; i >= 0; i--) {
        p[i] = h[v & 0xf];
        v >>= 4;
    }
}

static int sig_diag_put(char *b, int n, const char *s) {
    while (*s) b[n++] = *s++;
    return n;
}

static int sig_diag_put_hex(char *b, int n, const char *k, uint64_t v) {
    n = sig_diag_put(b, n, k);
    n = sig_diag_put(b, n, "0x");
    sig_diag_hex16(b + n, v);
    return n + 16;
}

static void sig_diag_fatal_fault(int sig, int hostsig, siginfo_t *si, struct cpu *c) {
    if (!getenv("DD_FATALSIG_LOG")) return;
    char b[384];
    int n = 0;
    n = sig_diag_put(b, n, "[DDFATAL]");
    n = sig_diag_put_hex(b, n, " pid=", (uint64_t)getpid());
    n = sig_diag_put_hex(b, n, " cpid=", (uint64_t)container_pid());
    n = sig_diag_put_hex(b, n, " sig=", (uint64_t)sig);
    n = sig_diag_put_hex(b, n, " hostsig=", (uint64_t)hostsig);
    n = sig_diag_put_hex(b, n, " pc=", c ? G_PC(c) : 0);
    n = sig_diag_put_hex(b, n, " sp=", c ? G_SP(c) : 0);
#if G_GPC_HASH_SHIFT == 2
    n = sig_diag_put_hex(b, n, " lr=", c ? c->x[30] : 0);
    n = sig_diag_put_hex(b, n, " x0=", c ? c->x[0] : 0);
    n = sig_diag_put_hex(b, n, " x1=", c ? c->x[1] : 0);
    n = sig_diag_put_hex(b, n, " x20=", c ? c->x[20] : 0);
#endif
    n = sig_diag_put_hex(b, n, " si_addr=", si ? (uint64_t)si->si_addr : 0);
    b[n++] = '\n';
    (void)write(2, b, (size_t)n);
}

static void sig_diag_sync_reraise(int sig, int ls, siginfo_t *si, void *ucv) {
    if (!getenv("DD_FATALSIG_LOG")) return;
    ucontext_t *u = (ucontext_t *)ucv;
#if defined(__aarch64__)
    uint64_t hpc = u ? (uint64_t)u->uc_mcontext->__ss.__pc : 0;
#elif defined(__x86_64__)
    uint64_t hpc = u ? (uint64_t)u->uc_mcontext->__ss.__rip : 0;
#else
    uint64_t hpc = 0;
#endif
    uint64_t hgpc = 0, hoff = 0;
    uint32_t hinsn = 0;
    extern int jit_hostpc_lookup(uint64_t hpc, uint64_t *gpc, uint64_t *off, uint32_t *insn);
    int hit = jit_hostpc_lookup(hpc, &hgpc, &hoff, &hinsn);
    struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
    char b[512];
    int n = 0;
    n = sig_diag_put(b, n, "[DDSYNC]");
    n = sig_diag_put_hex(b, n, " pid=", (uint64_t)getpid());
    n = sig_diag_put_hex(b, n, " cpid=", (uint64_t)container_pid());
    n = sig_diag_put_hex(b, n, " hostsig=", (uint64_t)sig);
    n = sig_diag_put_hex(b, n, " sig=", (uint64_t)ls);
    n = sig_diag_put_hex(b, n, " hpc=", hpc);
    n = sig_diag_put_hex(b, n, " pc=", c ? G_PC(c) : 0);
    n = sig_diag_put_hex(b, n, " sp=", c ? G_SP(c) : 0);
#if G_GPC_HASH_SHIFT == 2
    n = sig_diag_put_hex(b, n, " lr=", c ? c->x[30] : 0);
#endif
    n = sig_diag_put_hex(b, n, " si_addr=", si ? (uint64_t)si->si_addr : 0);
    n = sig_diag_put_hex(b, n, " jhit=", (uint64_t)hit);
    if (hit) {
        n = sig_diag_put_hex(b, n, " hblk=", hgpc);
        n = sig_diag_put_hex(b, n, " hoff=", hoff);
        n = sig_diag_put_hex(b, n, " hinsn=", (uint64_t)hinsn);
    }
    b[n++] = '\n';
    (void)write(2, b, (size_t)n);
}

static void sig_diag_raise_default(struct cpu *c, int sig) {
    if (!getenv("DD_FATALSIG_LOG")) return;
    char b[384];
    int n = 0;
    n = sig_diag_put(b, n, "[DDRAISE]");
    n = sig_diag_put_hex(b, n, " pid=", (uint64_t)getpid());
    n = sig_diag_put_hex(b, n, " cpid=", (uint64_t)container_pid());
    n = sig_diag_put_hex(b, n, " tid=", c ? (uint64_t)cpu_tid(c) : 0);
    n = sig_diag_put_hex(b, n, " sig=", (uint64_t)sig);
    n = sig_diag_put_hex(b, n, " pc=", c ? G_PC(c) : 0);
    n = sig_diag_put_hex(b, n, " sp=", c ? G_SP(c) : 0);
#if G_GPC_HASH_SHIFT == 2
    n = sig_diag_put_hex(b, n, " lr=", c ? c->x[30] : 0);
#endif
    n = sig_diag_put_hex(b, n, " handler=", (sig >= 1 && sig <= 64) ? g_sigact[sig].handler : 0);
    n = sig_diag_put_hex(b, n, " mask=", c ? c->sigmask : 0);
    b[n++] = '\n';
    (void)write(2, b, (size_t)n);
}

// a GENUINE synchronous CPU fault (SIGSEGV/SIGBUS/...) taken in translated code for which the guest
// installed NO handler. Such a fault is fatal and cannot be masked or ignored (a stack overflow into the
// guard gap, a wild pointer, a NULL deref). Terminate the guest process the SAME way dd terminates any
// fatal-default signal, so the exit status crosses dd's fork faithfully: the container init ends with
// 128+signo; a non-init guest records the intended Linux termination signal (sigexit_record) then exits
// via the normal c->exited path so its parent's wait4/waitid reconstructs WIFSIGNALED/WTERMSIG=signo. A
// raw host raise() cannot carry the signo across dd's fork and, from a MAP_JIT thread, degrades to a plain
// exit(255) (the parent then wrongly sees WIFEXITED, not WIFSIGNALED). Called by the per-arch SIGSEGV/SIGBUS
// guard AFTER deliver_guest_fault (the guest-handler path) declines. Returns 1 iff this was a genuine
// in-translated-code guest fault (caller stops); 0 for an engine fault / external async signal, so the
// caller re-raises the real crash unchanged.
static int deliver_guest_fatal_fault(int hostsig, siginfo_t *si, void *ucv) {
    int sig = sig_m2l(hostsig);
    if (sig < 1 || sig > 64 || !ucv) return 0;
    // Bad-address fault normalization (see deliver_guest_fault): macOS raises a PROT_NONE/guard access as host
    // SIGBUS -> Linux SIGBUS(7), but Linux reports SIGSEGV. Rewrite to SIGSEGV(11) for a guard/unmapped fault.
    if (hostsig == SIGBUS && si && si->si_addr &&
        (gna_hit((uint64_t)si->si_addr, 1) || !host_addr_mapped((uintptr_t)si->si_addr)))
        sig = 11;
    if (g_sigact[sig].handler > 1) return 0; // a guest handler exists -> not ours (deliver_guest_fault owns it)
    struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
    if (!c) return 0;
    if (!sigframe_capture_fault(c, ucv)) return 0; // host PC not in translated code -> engine/async: re-raise
    sig_diag_fatal_fault(sig, hostsig, si, c);
    // A genuine, fatal, unmaskable guest fault. Terminate the guest process HERE (async-signal-safe _exit),
    // not by resuming the dispatcher: the guest state is captured mid-fault (e.g. SP overrun into the guard),
    // so re-entering the code cache would run off into garbage. A non-init guest records its Linux
    // termination signo so the parent's wait4/waitid reconstructs WIFSIGNALED/WTERMSIG=sig (proc.c case 260);
    // the container init just exits 128+signo (what `docker run` reports for a crash). This is dd's standard
    // fatal-signal relay -- the same mechanism as a fatal-default signal in maybe_deliver_signal.
    if (container_pid() != 1) {
        int core = sig_coredumps(sig) && svc_core_rlimit_cur() > 0;
        sigexit_record(sig, core);
    }
    _exit(128 + sig);
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
