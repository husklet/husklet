// hl/linux_abi -- signal delivery (Linux<->macOS signal-number translation; sigframe build).
#include "../host/native_context.h"
#include "shared.h"

#ifndef HL_DISPATCH_FAULT_ADDRESS
#define HL_DISPATCH_FAULT_ADDRESS(c) nonpie_unfold((c)->fault_addr)
#endif

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
    uint64_t handler, flags, restorer, mask;
} g_sigact[65];

// ---------------- Go async-preempt SIGURG suppression for Go images (#423) ----------------
// Go's scheduler asynchronously preempts a running goroutine by sending itself SIGURG (23) and injecting a
// call to runtime.asyncPreempt from the signal handler. Delivering SIGURG into a translated aarch64 Go binary
// crashes it -- but NOT via a sigframe/SP overlap (that hypothesis was investigated with the
// go_cgo_stackgrow_arm fixture and DISPROVEN: build_signal_frame captures a correct, consistent guest SP/PC/LR
// and the frame sits strictly below SP). The real defect is ASYNC-PREEMPT SAFETY: the engine delivers the
// caught signal at translation-block boundaries (the cpu->irq poll), which do NOT coincide with Go's
// compiler-inserted async-safe points. Go's own guard (doSigPreempt -> isAsyncSafePoint / canPreemptM) is meant
// to no-op an unsafe delivery, but under the engine's non-PIE high-bias translation the interaction with Go's
// stack-growth machinery corrupts state: the cgo fixture's crash lands squarely in runtime.copystack /
// runtime.adjustframe / (*stkframe).getStackMap with `fatal error: wirep: already in go` and `untyped locals`;
// the INTERNAL-linked Go toolchain children `go build` forks (compile/asm/link) crash the same way -- a SIGURG
// delivered into sysmon's runtime.usleep SIGSEGVs (addr=0x0) and, under build parallelism, corrupts thread
// startup so clone/newosproc returns EAGAIN. A correct fix requires honoring Go's async-safe-point model
// (parsing the guest pclntab's PCDATA_UnsafePoint, or a safepoint-accurate delivery scheme) -- a large,
// fragile undertaking not yet landed. Until then we suppress SIGURG for EVERY Go image, which is functionally
// identical to Go's OWN supported `GODEBUG=asyncpreemptoff=1`: async (tight-loop) preemption is disabled, but
// COOPERATIVE preemption at safepoints still works, so the program runs correctly (proven: `go build` of a
// hello-world completes, influxd boots, vmetrics serves). This originally suppressed only the cgo
// (CGO_ENABLED=1 / runtime.iscgo==1) class; it now covers internal-linked Go too, which is exactly the toolchain
// children that were crashing.
//
// Scoped by Go-detection on purpose: g_go_image (set once by each load_elf, keyed on the linker's Go
// build-info magic -- elf_is_go_image in goimage.h) is 1 ONLY for a Go main image. It stays 0 for non-Go
// guests, some of which legitimately use SIGURG for OOB TCP data; a Go program never repurposes SIGURG, so
// dropping it is always safe for a Go image.
//
// BOTH engines latch it. The x86-64 engine did not, and an x86-64 Go guest therefore still received SIGURG:
// the failure mode there is not the aarch64 crash but a LIVELOCK. sysmon re-sends the preempt signal
// (getpid+tgkill, ~4k/s) because the block-boundary delivery never reaches an async-safe point, so the GC's
// stop-the-world never completes and the program runs forever at full CPU. Measured on the
// go-static-heapgc compat case: 13% of standalone runs hung past the 120s case budget having executed
// >500M translated blocks, against a 2.4s normal completion.
int g_go_image; // 1 iff the loaded main image is a Go binary; owned here, set by load_elf

// Should SIGURG (Go async-preempt) delivery be dropped for this process? The detected Go class is fixed
// before any signal fires.
static int sigurg_drop_enabled(void) {
    return g_go_image ? 1 : 0;
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
// Set by syscall_should_restart when an interrupted blocking syscall must be transparently RESTARTED after
// its pending SA_RESTART handler runs (it also sets c->redirect so the dispatcher re-executes the SVC).
// service() consumes it to restore the syscall's first argument register before returning: on aarch64 the
// arg0 and return registers alias (x0), so the result the handler code just wrote would otherwise be the
// "fd" the re-executed syscall sees. Distinct from the execve/sigreturn redirect (which sets a final x0).
static __thread int g_syscall_restart;
// rt_sigqueueinfo extras carried to the handler's siginfo: si_code + si_value (consumed on delivery)
static int g_sigcode[65];
static int g_sigerror[65];
static uint64_t g_sigval[65];
// SA_SIGINFO sender identity (si_pid/si_uid) captured from the host siginfo for a kill(2)/tgkill-delivered
// signal (consumed on delivery). g_sigpid==0 means "no sender identity" (async fault/internal), so a kill
// stamp is distinguishable from the sigfault si_addr that shares the same union offset.
static int g_sigpid[65];
static int g_siguid[65];
// synchronous-fault address carried to the handler's siginfo (si_addr; consumed on delivery, 0 for async)
static uint64_t g_sigaddr[65];
// Temporary Chrome child-fault diagnostics. Populated when the guest stack is built so fatal-default
// reports identify forked Chromium service roles whose executable path is otherwise identical.
static char g_fault_cmdline[512];

// ---------------- per-signal pending FIFO (siginfo carrier) ----------------
// g_pending/c->tpending stay the 1-bit-per-signal "is pending" indicators every fast-path scan reads.
// This queue carries the ORDERED per-instance siginfo (si_code/value/pid/uid/addr/status) that a single
// bit cannot represent: standard signals (1..31) coalesce to one entry, realtime signals (32..64) queue
// up to SIGQ_DEPTH instances FIFO. A g_pending bit is set whenever a signal's queue becomes non-empty
// (the in-process enqueue path); the host async handler path (host_sigh/host_sigh_si) may still set a bit
// with an EMPTY queue, in which case delivery falls back to the single-slot g_sig* arrays those handlers
// wrote. Delivery pops one queued instance into the g_sig* slots the per-arch frame builder reads.
// All queue operations run in guest-thread / dispatcher context (never a host signal handler), so a plain
// mutex is safe; host_sig* handlers deliberately never touch the ring.
#define SIGQ_DEPTH 64

struct sigq_ent {
    int error;      // si_errno (seccomp trap data)
    int code;       // si_code
    uint64_t value; // si_value (sigqueue) / si_status (SIGCHLD; aliases offset 24)
    int pid;        // si_pid
    int uid;        // si_uid
    uint64_t addr;  // si_addr
    int tag;        // source id (POSIX timer id + 1); 0 = untagged. Never reaches the guest siginfo.
};

static struct {
    struct sigq_ent e[SIGQ_DEPTH];
    int head, count;
} g_sigq[65];

static pthread_mutex_t g_sigq_lk = PTHREAD_MUTEX_INITIALIZER;

static int sig_is_rt(int s) {
    return s >= 32 && s <= 64;
}

// Enqueue one pending instance of Linux signal `sig`. Standard signals coalesce (keep the first queued
// siginfo, drop extras -- matching Linux non-RT coalescing); realtime signals queue FIFO up to
// SIGQ_DEPTH. Always sets the g_pending bit so every existing pending scan sees the signal.
// Returns 1 iff a new instance was actually enqueued -- the signalfd wake byte must be written ONLY then,
// or a coalesced-away duplicate leaves a spare byte in the self-pipe and signalfd reports one siginfo
// record too many (a second read that Linux answers with EAGAIN).
static int sigq_push(int sig, int tag, int error, int code, uint64_t value, int pid, int uid, uint64_t addr) {
    if (sig < 1 || sig > 64) return 0;
    int queued = 0;
    pthread_mutex_lock(&g_sigq_lk);
    int cap = sig_is_rt(sig) ? SIGQ_DEPTH : 1;
    if (g_sigq[sig].count < cap) {
        int t = (g_sigq[sig].head + g_sigq[sig].count) % SIGQ_DEPTH;
        g_sigq[sig].e[t] = (struct sigq_ent){
            .error = error,
            .code = code,
            .value = value,
            .pid = pid,
            .uid = uid,
            .addr = addr,
            .tag = tag,
        };
        g_sigq[sig].count++;
        queued = 1;
    }
    pthread_mutex_unlock(&g_sigq_lk);
    __atomic_or_fetch(&g_pending, 1ull << sig, __ATOMIC_SEQ_CST);
    return queued;
}

// Is an instance tagged `tag` still queued for `sig`? Linux queues at most ONE signal per POSIX timer --
// further expirations of the SAME timer raise timer_getoverrun instead of a second delivery -- while two
// DISTINCT timers sharing one signo each carry their own instance. A bare pending bit cannot express that.
static int sigq_tag_queued(int sig, int tag) {
    if (sig < 1 || sig > 64) return 0;
    int found = 0;
    pthread_mutex_lock(&g_sigq_lk);
    for (int i = 0; i < g_sigq[sig].count; i++)
        if (g_sigq[sig].e[(g_sigq[sig].head + i) % SIGQ_DEPTH].tag == tag) {
            found = 1;
            break;
        }
    pthread_mutex_unlock(&g_sigq_lk);
    return found;
}

// Discard queued instances tagged `tag` (Linux drops a timer's pending signal at timer_delete). Timer ids
// are reused array slots, so a stale entry would otherwise coalesce away the NEW timer's first expiry.
static void sigq_drop_tag(int sig, int tag) {
    if (sig < 1 || sig > 64 || tag == 0) return;
    pthread_mutex_lock(&g_sigq_lk);
    int kept = 0;
    struct sigq_ent copy[SIGQ_DEPTH];
    for (int i = 0; i < g_sigq[sig].count; i++) {
        struct sigq_ent e = g_sigq[sig].e[(g_sigq[sig].head + i) % SIGQ_DEPTH];
        if (e.tag != tag) copy[kept++] = e;
    }
    int dropped = g_sigq[sig].count - kept;
    for (int i = 0; i < kept; i++)
        g_sigq[sig].e[i] = copy[i];
    g_sigq[sig].head = 0;
    g_sigq[sig].count = kept;
    pthread_mutex_unlock(&g_sigq_lk);
    // Only when this call emptied the queue: an untouched empty queue may still have a bit set by the host
    // async path, which carries its siginfo in the single-slot g_sig* arrays.
    if (dropped > 0 && kept == 0) __atomic_and_fetch(&g_pending, ~(1ull << sig), __ATOMIC_SEQ_CST);
}

// Pop the oldest queued instance of `sig` into *out. Returns 1 iff one was dequeued; clears the g_pending
// bit when the queue drains so a realtime signal keeps its bit set while further instances remain.
static int sigq_pop(int sig, struct sigq_ent *out) {
    if (sig < 1 || sig > 64) return 0;
    int got = 0;
    pthread_mutex_lock(&g_sigq_lk);
    if (g_sigq[sig].count > 0) {
        *out = g_sigq[sig].e[g_sigq[sig].head];
        g_sigq[sig].head = (g_sigq[sig].head + 1) % SIGQ_DEPTH;
        g_sigq[sig].count--;
        got = 1;
        if (g_sigq[sig].count == 0) __atomic_and_fetch(&g_pending, ~(1ull << sig), __ATOMIC_SEQ_CST);
    }
    pthread_mutex_unlock(&g_sigq_lk);
    return got;
}

// Discard every queued instance of `sig` and clear its pending bit (Linux discards pending on SIG_IGN).
static void sigq_flush(int sig) {
    if (sig < 1 || sig > 64) return;
    pthread_mutex_lock(&g_sigq_lk);
    g_sigq[sig].head = g_sigq[sig].count = 0;
    pthread_mutex_unlock(&g_sigq_lk);
    __atomic_and_fetch(&g_pending, ~(1ull << sig), __ATOMIC_SEQ_CST);
}

// sentinel lr: handler return -> sigreturn
#define SIGRETURN_PC 0xFFFFFFFFFFF0ull

static int sig_is_sync(int s) {
    return s == 4 || s == 5 || s == 7 || s == 8 || s == 11;
    // ILL TRAP BUS FPE SEGV (Linux nums)
}

// Native Linux has no signal numbers outside the Linux guest ABI. The two
// host signals selected for engine control therefore remain virtual for the
// guest: guest dispositions live in g_sigact and must never replace these
// process-wide host handlers.
static int sig_host_is_engine_control(int hostsig) {
    return hostsig == STW_SIG || hostsig == THREAD_INT_SIG;
}

#if defined(__linux__)
#define HOST_SIGNAL_HAS_FAULT_ADDRESS(si) ((si) != NULL && (si)->si_code > 0)
#else
#define HOST_SIGNAL_HAS_FAULT_ADDRESS(si) ((si) != NULL)
#endif

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
// setrlimit/prlimit64 store (g_limits, seeded in state.c) wins, else the engine default. A core dump only
// happens when a coredumping signal kills a process whose SOFT core limit is nonzero, so this is the single
// input WCOREDUMP is gated on. g_limits comes from container/state.c (included first).
// The default MUST be 0 (cores OFF), matching getrlimit(RLIMIT_CORE)'s Linux/docker default soft=0 -- the
// old RLIM_INFINITY default contradicted getrlimit and made every crash report WCOREDUMP even though cores
// were disabled (wait4/waitid reported CLD_DUMPED while getrlimit said the core limit was 0).
static uint64_t svc_core_rlimit_cur(void) {
    uint64_t current;
    if (hl_limit_table_get(&g_limits, 4, &current, NULL)) return current;
    return 0; // Linux/docker default: cores OFF (soft RLIMIT_CORE = 0)
}

// ---------------- guest signal-death relay ----------------
// Every guest process is a real host (macOS) process, so a guest parent reaps its children with the host
// wait4/waitid and reads the host termination status. When a guest child must die from a fatal-default
// signal, hl normally lets it die BY the mapped host signal, and the parent's wait4 translates the host
// termsig back (sig_m2l). That fails for signals with NO faithful fatal host mapping: SIGPOLL(29)->host
// SIGIO / SIGSTKFLT(16)->host SIGURG both DEFAULT-IGNORE on macOS (so raising them does not terminate),
// and SIGPWR(30)->host SIGUSR1 maps BACK to a different signo (10). hl then fell back to _exit(128+signo),
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
    void *arena = NULL;
    if (g_sigexit) return;
    if (hl_linux_shared_create(effective_host_services(), sizeof(struct sigexit_ent) * SIGEXIT_SLOTS, &arena) ==
        HL_STATUS_OK)
        g_sigexit = (struct sigexit_ent *)arena;
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
#if defined(__linux__)
    return s;
#else
    static const unsigned char T[32] = {0,  1,  2,  3,  4,  5,  6,  10, 8,  9,  30, 11, 31, 13, 14, 15,
                                        16, 20, 19, 17, 18, 21, 22, 16, 24, 25, 26, 27, 28, 23, 30, 12};
    return (s >= 1 && s <= 31) ? T[s] : s;
#endif
}

static int sig_m2l(int s) {
/* _WIN32 joins the identity arm, not the BSD table. There is no host signal
 * numbering to translate FROM on that host: nothing delivers a host signal
 * there, and the only status words that reach this function are ones this
 * engine minted itself in Linux numbering (the backend's reap decodes an NT
 * exception status straight into a Linux signo). Running them through the BSD
 * table would renumber e.g. 30 -> 10 for no host that ever said 30. */
#if defined(__linux__) || defined(_WIN32)
    return s;
#else
    static const unsigned char T[32] = {0,  1,  2,  3,  4,  5,  6,  7,  8,  9,  7,  11, 31, 13, 14, 15,
                                        23, 19, 20, 18, 17, 21, 22, 29, 24, 25, 26, 27, 28, 29, 10, 12};
    return (s >= 1 && s <= 31) ? T[s] : s;
#endif
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
// hl's old model was a SINGLE shared self-pipe with one ORed mask, which failed exactly that independence.
// Each signalfd OFD is now one slot here: a self-pipe (read end = the guest's signalfd, write end poked by
// the host signal handlers) + its own mask + a refcount (a dup(2) shares the OFD, so refs bumps and the
// pipe is torn down only when the last alias closes). The read end is a normal guest fd; only the write end
// is engine-private (relocated out of the guest's low fd range at create, protected from the guest's
// close/exec sweep). `g_sigfd_slot[fdnum]` maps a guest fd NUMBER to its OFD slot (+1); 0 = not a signalfd.
#define HL_SFD_MAX 64

struct sfd_ofd {
    int rd;                 // read end (a guest fd number)
    int wr;                 // write end (engine-private, poked on signal delivery)
    volatile uint64_t mask; // signals routed to THIS signalfd (1<<signo)
    int refs;               // fd aliases referring to this OFD (dup bumps); 0 = free slot
};
static struct sfd_ofd g_sfd[HL_SFD_MAX];
static uint8_t g_sigfd_slot[HL_NFD]; // guest fd number -> OFD slot index + 1 (0 = not a signalfd)

// Allocate a free OFD slot (refs==0). Returns the slot index or -1 if the pool is exhausted.
static int sfd_alloc(void) {
    for (int i = 0; i < HL_SFD_MAX; i++)
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
    for (int i = 0; i < HL_SFD_MAX; i++)
        if (g_sfd[i].refs > 0 && g_sfd[i].wr >= 0 && (g_sfd[i].mask & bit)) {
            char b = (char)ls;
            if (write(g_sfd[i].wr, &b, 1) < 0) {}
        }
}

// Is Linux signal `ls` routed to at least one live signalfd (so a blocked instance must be captured for
// its read queue rather than merely left pending for a future handler run)?
static int sfd_routed(int ls) {
    if (ls < 1 || ls > 63) return 0;
    uint64_t bit = 1ull << ls;
    for (int i = 0; i < HL_SFD_MAX; i++)
        if (g_sfd[i].refs > 0 && g_sfd[i].wr >= 0 && (g_sfd[i].mask & bit)) return 1;
    return 0;
}

// Is host fd `fd` a signalfd write end? (engine-private -- must survive the guest's close/exec sweep.)
static int sfd_wr_is(int fd) {
    if (fd < 0) return 0;
    for (int i = 0; i < HL_SFD_MAX; i++)
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
static int deliver_guest_fault(int hostsig, siginfo_t *si, void *ucv);

static _Noreturn void guest_group_fatal(struct cpu *c, int sig) {
    sig_diag_raise_default(c, sig);
    if (container_pid() != 1) {
        int core = sig_coredumps(sig) && svc_core_rlimit_cur() > 0;
        sigexit_record(sig, core);
    }
    hl_engine_child_result_publish_signal(sig);
    _exit(128 + sig);
}

// SA_SIGINFO host handler: same delivery as host_sigh, plus it captures the sender's pid/uid so an
// SA_SIGINFO guest handler (or sigwaitinfo) sees si_pid/si_uid. macOS populates si_pid for a kill(2) but
// does NOT set the Linux SI_USER si_code, so gate on si_pid>0 (a real sender) rather than the code.
static void host_sigh_si(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    int ls = sig_m2l(sig);
    // Linux coalesces a standard (non-realtime) signal that is ALREADY pending: the first siginfo is
    // retained and any later instance is dropped (kernel legacy_queue()). SIGCHLD depends on this -- a child
    // that is continued (CLD_CONTINUED, from SIGCONT) and then exits (CLD_EXITED) in quick succession must
    // deliver the FIRST (continued) siginfo to the parent, not have it overwritten by the exit. Our
    // single-slot g_sig* store would otherwise clobber the still-pending continued notification with the
    // exit on a fast host (deterministically flaky on native aarch64), so keep the first while the pending
    // bit is set. (A realtime signal keeps every instance via sigq_push and is unaffected.)
    int chld_keep_first = (ls == 17) && ((__atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) >> 17) & 1);
    if (si && si->si_pid > 0 && !chld_keep_first) {
        g_sigpid[ls] = (int)si->si_pid;
        g_siguid[ls] = (int)si->si_uid;
    }
#if defined(__linux__)
    // A Linux host delivers the real Linux si_code, so forward it (and, for the queued sources that carry a
    // payload, si_value) into the single-slot siginfo the frame builder reads. This is what lets a kernel
    // POSIX-mqueue notification (mq_notify SIGEV_SIGNAL, host-forwarded in rare.c) reach the guest handler
    // as SI_MESGQ with the registered sigev_value, and likewise carries a cross-process sigqueue (SI_QUEUE)
    // or POSIX-AIO (SI_ASYNCIO) value. SIGCHLD keeps its dedicated si_status handling below (si_status
    // aliases si_value). SI_USER/SI_KERNEL/SI_TKILL carry no value, so only the value-bearing negative codes
    // copy si_value (its union slot is meaningful only then); a plain kill stays SI_USER/0.
    if (si && ls != 17) {
        g_sigcode[ls] = si->si_code;
        if (si->si_code == SI_QUEUE || si->si_code == SI_TIMER || si->si_code == SI_MESGQ || si->si_code == SI_ASYNCIO)
            g_sigval[ls] = (uint64_t)(uintptr_t)si->si_value.sival_ptr;
    }
#endif
    // SA_SIGINFO SIGCHLD exposes HOW the child ended: si_code (CLD_EXITED/CLD_KILLED/...) and si_status
    // (exit code or terminating signal). On a Linux host the host siginfo already carries the Linux CLD_*
    // code and status, so forward them into the single-slot siginfo the frame builder reads (si_status
    // aliases si_value at offset 24). Leaving these zero made a guest handler see code==0/status==0.
    if (ls == 17 && si && !chld_keep_first) {
        g_sigcode[17] = si->si_code;
        g_sigval[17] = (uint64_t)(uint32_t)si->si_status;
    }
    // SA_NOCLDWAIT on the guest's SIGCHLD: Linux still DELIVERS the SIGCHLD but leaves no zombie. macOS's own
    // SA_NOCLDWAIT would suppress the signal entirely, so we don't set it (see rt_sigaction) -- instead
    // auto-reap every terminated child here (WNOHANG, and no WUNTRACED so a stopped child is left alone). The
    // guest handler still runs (host_sig_pend below) and a later wait() sees ECHILD. Gated on the guest opt-in.
    if (ls == 17 && (g_sigact[17].flags & 0x2)) {
        int wst;
        while (waitpid(-1, &wst, WNOHANG) > 0) {}
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
        // Linux/AArch64 delivers translated illegal instructions through this POSIX handler. Route a
        // code-cache fault to the guest's installed SIGILL handler (OpenSSL/V8 feature probes) first.
        if (deliver_guest_fault(sig, si, uc)) return;
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

// build_signal_frame + do_sigreturn are per-arch -> translator/guest/<arch>/signal.c
static void build_signal_frame(struct cpu *c, int sig, int synchronous);
static void do_sigreturn(struct cpu *c);
// per-arch (the host<->guest register model differs): on a synchronous fault inside translated code,
// reconstruct the guest register state from the host fault context (returns 1 iff the faulting host PC is
// in the code cache), and steer the host context back into the dispatcher so a guest handler can run.
static int sigframe_capture_fault(struct cpu *c, void *ucv);
static void sigframe_resume_dispatch(struct cpu *c, void *ucv);

// The delivery fold's mirror: at a handler return the guest SP IS the ucontext the builder handed over, so
// for a non-PIE alt stack inside the image it is a LOW link address the restore would dereference. Fold for
// the read, and put the restored SP back into guest coordinates. Every rt_sigreturn goes through here.
static void sigreturn_frame(struct cpu *c) {
    G_SP(c) = nonpie_fold(G_SP(c));
    do_sigreturn(c);
    G_SP(c) = nonpie_unfold(G_SP(c));
}

static void maybe_deliver_signal(struct cpu *c) {
    // Two sources: g_pending (process-directed -- any thread may take it) and c->tpending (thread-directed
    // via tkill/tgkill -- only THIS thread). Consider both; coalescing a process- and thread-directed
    // instance of the same (non-realtime) signal into one delivery is the correct Linux semantics.
    // A handler may leave without an rt_sigreturn (siglongjmp out of a fault/signal handler restores the guest
    // registers directly). Detect that by the guest SP unwinding back ABOVE a recorded frame: pop those levels
    // so their deferred signals are released and the defer stack cannot leak. (The guest stack grows down, so a
    // live handler's SP is <= its frame base; a longjmp back to an outer context raises SP above it.)
    while (c->sig_depth > 0 && G_SP(c) > c->sig_frame_sp[c->sig_depth - 1]) {
        c->sig_depth--;
        c->sig_defer = c->sig_depth > 0 ? c->sig_defer_stack[c->sig_depth] : 0;
    }
    uint64_t p = __atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST);
    // Delivery order matches the native kernel: when several signals are pending together, the
    // HIGHEST-numbered deliverable one runs first (verified against native aarch64 for both standard
    // signals -- blocked_delivery_order 15,12,10 -- and realtime signals -- rt_signal_order highest
    // signo first, FIFO within a signo). Scan high->low; realtime instances are dequeued FIFO per signo.
    for (int sig = 64; sig >= 1; sig--) {
        uint64_t bit = 1ull << sig;
        // sigmask is sigset_t (bit N-1). A signal blocked by the mask is normally not delivered -- UNLESS it
        // was force-marked by rt_sigsuspend/pause (POSIX: the awaited handler runs during the suspend even
        // though the restored mask blocks it). g_force_deliver overrides the mask for exactly that one bit.
        if (!(p & bit)) continue;
        // INTERIM: suppress Go's async-preempt SIGURG (23) for a Go image (see the note at the
        // top of this file). Drop the pending instance from both queues so it is never delivered to the guest
        // handler; cooperative preemption keeps the program correct. Scoped to exactly the Go-image class.
        if (sig == 23 && sigurg_drop_enabled()) {
            __atomic_and_fetch(&g_pending, ~bit, __ATOMIC_SEQ_CST);
            __atomic_and_fetch(&c->tpending, ~bit, __ATOMIC_SEQ_CST);
            continue;
        }
        if ((c->sigmask & (1ull << (sig - 1))) && !(g_force_deliver & bit)) continue;
        // Deferred: this signal was already pending when the current handler was entered, so it waits until
        // that handler returns (native delivers a batch of unblocked signals serially, not nested). A signal
        // raised DURING the handler is not in c->sig_defer and still nests. Force-delivery overrides.
        if ((c->sig_defer & bit) && !(g_force_deliver & bit)) continue;
        uint64_t h = g_sigact[sig].handler;
        if (h <= 1) {
            // No guest handler -- discard every pending instance from all queues (and any force mark).
            g_force_deliver &= ~bit;
            sigq_flush(sig);
            __atomic_and_fetch(&g_pending, ~bit, __ATOMIC_SEQ_CST);
            __atomic_and_fetch(&c->tpending, ~bit, __ATOMIC_SEQ_CST);
            // A SIG_DFL signal whose default action TERMINATES, still pending at the container init, was NOT
            // already actioned by the host: real Linux protects a PID-namespace init from an unhandled fatal
            // signal, so it lingered (e.g. the guest blocked it inside its handler, reset the disposition to
            // SIG_DFL, then re-raised it to exit -- exactly node's SignalExit / mongosh path). hl's init is
            // just the container entrypoint, not an init that must survive, so take the default action and end
            // the container with 128+signo (the code `docker run` reports for a PID 1 killed by a signal).
            // SIG_IGN (h==1) and the default-ignore/stop signals stay dropped here.
            if (h == 0 && container_pid() == 1 && sig_default_terminates(sig)) { guest_group_fatal(c, sig); }
            continue;
        }
        // Claim ONE instance and run the guest handler on this thread. Pop the per-instance siginfo from
        // the FIFO into the single-slot g_sig* arrays the frame builder reads; a realtime signal keeps its
        // g_pending bit set (sigq_pop clears it only when the queue drains) so the next instance is
        // delivered after this handler returns. The thread-directed bit (synchronous faults, tkill) has no
        // queue -- clear it directly. If nothing was actually queued (host async path set the bit with an
        // empty queue), fall back to clearing the process bit and using whatever g_sig* the host wrote.
        struct sigq_ent ent;
        int popped = sigq_pop(sig, &ent);
        uint64_t had_t = __atomic_fetch_and(&c->tpending, ~bit, __ATOMIC_SEQ_CST) & bit;
        uint64_t had_p = 0;
        if (!popped) had_p = __atomic_fetch_and(&g_pending, ~bit, __ATOMIC_SEQ_CST) & bit;
        if (popped || had_t || had_p) {
            g_force_deliver &= ~bit; // consumed: the sigframe (built below) saves the true post-suspend mask
            if (popped) {
                g_sigerror[sig] = ent.error;
                g_sigcode[sig] = ent.code;
                g_sigval[sig] = ent.value;
                g_sigpid[sig] = ent.pid;
                g_siguid[sig] = ent.uid;
                g_sigaddr[sig] = ent.addr;
            }
            // Defer every OTHER signal pending right now until this handler returns: they were pending
            // before it started, so they must run after it (serial priority order), not nest inside it.
            // Push the enclosing level's deferred set; a signal raised during this handler is not captured
            // here, so it still nests. (The bit for `sig` is excluded so a realtime signal's further queued
            // instances -- whose g_pending bit is still set -- deliver after this handler returns.)
            if (c->sig_depth < (int)(sizeof c->sig_defer_stack / sizeof c->sig_defer_stack[0])) {
                c->sig_defer_stack[c->sig_depth] = c->sig_defer;
                c->sig_depth++;
                c->sig_defer |=
                    (__atomic_load_n(&g_pending, __ATOMIC_SEQ_CST) | __atomic_load_n(&c->tpending, __ATOMIC_SEQ_CST)) &
                    ~bit;
            }
            uint64_t flags = g_sigact[sig].flags;
            int synchronous = had_t && c->sync_signal == sig;
            // Non-PIE coordinates (see thread.c): the frame builder writes the sigframe THROUGH the address
            // it derives from the alt stack / current SP, so a `.bss` sigaltstack inside an ET_EXEC -- a LOW
            // link vaddr whose bytes live at +bias -- killed the engine before the handler's first
            // instruction. Fold the two inputs the base comes from, then put every address the builder hands
            // BACK (SP, the siginfo and ucontext arguments, uc_stack.ss_sp) into guest coordinates again.
            uint64_t alt_guest = c->alt_sp;
            c->alt_sp = nonpie_fold(alt_guest);
            G_SP(c) = nonpie_fold(G_SP(c));
            build_signal_frame(c, sig, synchronous);
            c->alt_sp = alt_guest;
            if (nonpie_unfold(G_A2(c)) != G_A2(c)) {
                // uc_stack.ss_sp is at ucontext+16 on every Linux LP64 arch; the builder filled it from the
                // folded alt_sp. Rewrite it while G_A2 still names the storage.
                *(uint64_t *)(uintptr_t)(G_A2(c) + 16) = alt_guest;
                G_SP(c) = nonpie_unfold(G_SP(c));
                G_A1(c) = nonpie_unfold(G_A1(c));
                G_A2(c) = nonpie_unfold(G_A2(c));
            }
            // Record this handler frame's guest SP so a siglongjmp unwind (which never calls rt_sigreturn)
            // can be detected at the next delivery and the defer level released.
            if (c->sig_depth > 0) c->sig_frame_sp[c->sig_depth - 1] = G_SP(c);
            if (synchronous) {
                c->sync_signal = 0;
                c->sync_code = 0;
                c->sync_address = 0;
            }
            // SA_RESETHAND (SA_ONESHOT, 0x80000000): the disposition reverts to SIG_DFL after this single
            // delivery (the handler PC is already baked into the frame above). Reset both the recorded
            // disposition and the emulated host disposition, so a second occurrence takes the default action
            // (LTP-style signal()-with-caller-reset semantics; glibc's legacy signal() sets SA_RESETHAND).
            if (flags & 0x80000000ull) {
                g_sigact[sig].handler = 0;
                if (sig != 9 && sig != 19 && !sig_is_sync(sig) && !sig_host_is_engine_control(sig_l2m(sig)))
                    signal(sig_l2m(sig), SIG_DFL);
            }
            return;
        }
    }
}

// A signal aimed at our own process (raise/abort/pthread_kill/kill-self/sigqueue). Deliver it through our
// own machinery instead of a real host signal (host signals into a MAP_JIT thread are fragile): a guest
// handler / blocked -> queue the per-instance siginfo + pending bit; otherwise apply the default action.
// `code`/`value`/`pid`/`uid` are the siginfo to carry (SI_USER + sender pid for a plain kill/raise, or
// SI_QUEUE + value + sender pid for sigqueue); realtime signals queue every instance FIFO.
static void raise_guest_signal_info(struct cpu *c, int sig, int error, int code, uint64_t value, int pid, int uid,
                                    uint64_t address) {
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
    int blocked = c && (c->sigmask & (1ull << (sig - 1)));
    // A blocked signal routed to a signalfd is captured for that fd's read queue regardless of its
    // handler disposition (Linux delivers a blocked signal to signalfd, not to a handler). Feed the
    // self-pipe (readability) AND queue the siginfo (ssi_int/pid/code); the read path drains it in order.
    if (blocked && sfd_routed(sig)) {
        if (sigq_push(sig, 0, error, code, value, pid, uid, address)) sfd_deliver(sig);
        return;
    }
    // custom handler -> queue for the dispatcher's maybe_deliver_signal (carries per-instance siginfo)
    if (h > 1) {
        sigq_push(sig, 0, error, code, value, pid, uid, address);
        return;
    }
    // SIG_IGN
    if (h == 1) return;
    // blocked, no handler: queue pending for delivery on unblock (also feeds any signalfd via sfd_deliver)
    if (blocked) {
        if (sigq_push(sig, 0, error, code, value, pid, uid, address)) sfd_deliver(sig);
        return;
    }
    // SIGCHLD/CONT/URG/WINCH: ignore
    if (sig == 17 || sig == 18 || sig == 23 || sig == 28) return;
    // Non-init guest process dying from a fatal-default signal. A guest process IS a real host process, so
    // its parent reaps it with the host wait4/waitid. Raising the mapped HOST signal cannot faithfully carry
    // the Linux signo for every signal: SIGPOLL(29)/SIGSTKFLT(16) map to host signals that DEFAULT-IGNORE on
    // macOS (raise() then returns without terminating) and SIGPWR(30) maps to a host signal that reports a
    // DIFFERENT signo back (10). So instead of raising, record the intended Linux termination signal in the
    // shared relay and _exit(128+signo); the parent's wait4/waitid reconstructs WIFSIGNALED/WTERMSIG=sig
    // (proc.c case 260 / rare.c case 95). WCOREDUMP per Linux rules: a coredumping signal with soft
    // RLIMIT_CORE > 0. If the relay slot table is exhausted the parent simply sees the WIFEXITED(128+signo)
    // fallback — the same graceful degradation as before this fix.
    if (sig_default_terminates(sig)) { guest_group_fatal(c, sig); }
    // Non-terminating default reaching here = a stop signal (STOP/TSTP/TTIN/TTOU): mirror it onto the host so
    // a real job-control stop happens (the host mask mirrors these too — see rt_sigprocmask). A stop is NOT a
    // termination: the host process stops, the parent's waitpid(WUNTRACED) reaps the stop, and when a later
    // SIGCONT resumes it raise() returns 0 -- the guest must then RESUME execution from the stop point, not
    // exit. Setting c->exited here unconditionally forced the guest to terminate with 128+stopsig (e.g. 147
    // for SIGSTOP) the instant it was continued, so the parent's next wait saw a bogus WIFEXITED(0x9300)
    // instead of the child's real exit status. Only fall back to termination when raise() could not deliver
    // the stop (an invalid host signo returns nonzero).
    int host_stop = sig_l2m(sig);
    signal(host_stop, SIG_DFL);
    if (raise(host_stop) == 0) return; // stopped, then continued by SIGCONT -> resume guest execution
    c->exited = 1;
    c->exit_code = 128 + sig; // fallback: raise failed (signo invalid on host)
}

static void raise_guest_signal_si(struct cpu *c, int sig, int code, uint64_t value, int pid, int uid) {
    raise_guest_signal_info(c, sig, 0, code, value, pid, uid, 0);
}

// Convenience: a self-directed signal with no explicit sender info (raise/abort/kill-self/pthread_kill).
// Linux stamps si_code == SI_USER(0) and si_pid == the sending (== this) process; stamp the guest pid so
// an SA_SIGINFO handler / sigwaitinfo sees the correct sender (sigqueue_value's kill(2) leg).
static void raise_guest_signal(struct cpu *c, int sig) {
    // Linux stamps si_uid == the REAL uid of the sending process (getuid()), not a hardcoded 0. Use the
    // guest's modeled real uid (g_ruid: 0 for a root container, the launcher's uid in bare mode) so an
    // SA_SIGINFO handler / sigwaitinfo / signalfd all read the correct sender credential.
    cred_init();
    raise_guest_signal_si(c, sig, 0 /*SI_USER*/, 0, container_pid(), g_ruid);
}

// Linux delivers SIGPIPE to a guest thread whose write(2)/writev(2)/send(2)-without-MSG_NOSIGNAL hit a
// pipe or socket whose reader is gone -- the write returns EPIPE AND, unless SIGPIPE is ignored or blocked,
// a SIGPIPE is raised (default action: terminate, so `cmd | head` stops the writer). The host layer either
// delivers host SIGPIPE itself or (the container primary-channel path) blocks it and returns EPIPE, so a
// guest whose write returned EPIPE could otherwise never see SIGPIPE: it kept looping / printed "Broken
// pipe" and pipelines like `yes | head` hung. Own the delivery here, keyed off the guest disposition:
// SIG_IGN / blocked -> raise_guest_signal is a no-op and the EPIPE the caller already set stands; a handler
// -> it runs and the guest still gets EPIPE; SIG_DFL -> the writer is terminated. `ret` is the syscall
// result already computed by the caller (-EPIPE on the broken-pipe case). Idempotent if the host handler
// also marked SIGPIPE pending (same non-realtime bit coalesces into one delivery).
static void svc_sigpipe_on_epipe(struct cpu *c, int64_t ret) {
    if (c && ret == -(int64_t)EPIPE) raise_guest_signal(c, 13); // Linux SIGPIPE
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
    // macOS raises a PROT_NONE access / unmapped-page / guard-gap fault as host SIGBUS (-> Linux
    // SIGBUS(7)), whereas Linux reports those as SIGSEGV(11). On macOS, rewrite the host SIGBUS to
    // SIGSEGV unless the lock-free file-mapping BUS ledger identifies a real Linux past-EOF fault, so a
    // guest's own SIGSEGV handler (glibc stack-overflow detection, a JIT/VM's guard-page trap) catches it.
    // Do NOT query the Mach VM map from this synchronous signal handler: mach_vm_region is not
    // async-signal-safe; the ledger is the sole positive classification. A fault-class signal received
    // while blocked in a host service is asynchronous guest delivery and retains its signal number.
    //
    // This disambiguation is macOS-specific: only there is host SIGBUS overloaded across PROT_NONE guard
    // accesses AND real past-EOF bus errors, and only there is the ledger populated to tell them apart.
    // On a Linux host the guest runs on real host file mappings, so the kernel already raises host SIGBUS
    // exactly (and only) for genuine bus errors (past-EOF, misalignment) -- there the ledger is empty, so
    // the rewrite would wrongly downgrade every guest SIGBUS to SIGSEGV. Trust the host signo on Linux.
#if !defined(__linux__)
    if (hostsig == SIGBUS && !g_in_service && si && !hl_linux_bus_hit((uint64_t)si->si_addr, 1)) sig = 11;
#endif
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
    c->sync_signal = sig;
    // si_addr is GUEST-visible (a handler compares it against its own pointers, and a fault-recovery
    // handler resumes from it), but a hardware fault reports the STORAGE address -- so a fault inside a
    // non-PIE image handed the guest an address it has no name for. Unfold it; thread.c has the rule.
    // sync_code below deliberately keeps the raw host address: it asks the host mapping, not the guest.
    c->sync_address = si ? nonpie_unfold((uint64_t)si->si_addr) : 0;
    // Linux distinguishes an unmapped address (SEGV_MAPERR) from a mapped
    // protection violation (SEGV_ACCERR).  JIT safepoint/guard handlers use
    // that distinction; a physically protected g_gna page is ACCERR even
    // when Darwin surfaced the access as SIGBUS.
    c->sync_code = (sig == 7 || (sig == 11 && si &&
                                 (gna_hit((uint64_t)si->si_addr, 1) || host_addr_mapped((uintptr_t)si->si_addr))))
                       ? 2
                       : 1;
    c->sigmask &= ~(1ull << (sig - 1)); // a sync fault forces delivery even if the guest blocked it
    c->reason = R_BRANCH;               // resume as a plain branch (no stale syscall/special-op handling)
    __atomic_or_fetch(&c->tpending, 1ull << sig, __ATOMIC_SEQ_CST);
    sigframe_resume_dispatch(c, ucv);
    return 1;
}

// POSIX-guard entry: the faulting thread IS this thread, so its cpu is in TLS (cpu_hint == NULL).
static int deliver_guest_fault(int hostsig, siginfo_t *si, void *ucv) {
    return deliver_guest_fault_hint(NULL, hostsig, si, ucv);
}

/* Dispatcher-only delivery for a translated access rejected by the file-mapping BUS ledger. */
static int raise_guest_bus(struct cpu *c) {
    if (g_sigact[7].handler <= 1) { guest_group_fatal(c, 7); }
    c->sync_signal = 7;
    c->sync_address = HL_DISPATCH_FAULT_ADDRESS(c);
    c->sync_code = 2; /* BUS_ADRERR */
    c->sigmask &= ~(1ull << 6);
    c->reason = R_BRANCH;
    /* A synchronous memory fault belongs to the faulting thread.  Process-wide
       pending delivery can run the handler on an unrelated mapper thread. */
    __atomic_or_fetch(&c->tpending, 1ull << 7, __ATOMIC_SEQ_CST);
    return 1;
}

/* Dispatcher-only delivery for an instruction fetch rejected by a logical
   executable mapping.  Translation emits a runnable exit stub; the fault is
   therefore delivered in ordinary guest execution context, never by raising
   a host signal while the translator is reading source bytes. */
static int raise_guest_fetch_fault(struct cpu *c) {
    if (g_sigact[11].handler <= 1) { guest_group_fatal(c, 11); }
    c->sync_signal = 11;
    c->sync_address = HL_DISPATCH_FAULT_ADDRESS(c);
    c->sync_code = 2; /* SEGV_ACCERR */
    c->sigmask &= ~(1ull << 10);
    c->reason = R_BRANCH;
    __atomic_or_fetch(&c->tpending, 1ull << 11, __ATOMIC_SEQ_CST);
    return 1;
}

static int raise_guest_data_map_fault(struct cpu *c) {
    if (g_sigact[11].handler <= 1) guest_group_fatal(c, 11);
    c->sync_signal = 11;
    c->sync_address = HL_DISPATCH_FAULT_ADDRESS(c);
    c->sync_code = 1; /* SEGV_MAPERR */
    c->sigmask &= ~(1ull << 10);
    c->reason = R_BRANCH;
    __atomic_or_fetch(&c->tpending, 1ull << 11, __ATOMIC_SEQ_CST);
    return 1;
}

static void sig_diag_hex16(char *p, uint64_t v) {
    static const char h[] = "0123456789abcdef";
    for (int i = 15; i >= 0; i--) {
        p[i] = h[v & 0xf];
        v >>= 4;
    }
}

static int sig_diag_put(char *b, int n, const char *s) {
    while (*s)
        b[n++] = *s++;
    return n;
}

static int sig_diag_put_hex(char *b, int n, const char *k, uint64_t v) {
    n = sig_diag_put(b, n, k);
    n = sig_diag_put(b, n, "0x");
    sig_diag_hex16(b + n, v);
    return n + 16;
}

static void sig_diag_write(const char *buffer, size_t length) {
    while (length != 0) {
        ssize_t written = write(STDERR_FILENO, buffer, length);
        if (written > 0) {
            buffer += written;
            length -= (size_t)written;
        } else if (written < 0 && errno == EINTR) {
            continue;
        } else {
            break;
        }
    }
}

static void sig_diag_fatal_fault(int sig, int hostsig, siginfo_t *si, struct cpu *c, void *ucv) {
    char b[512];
    int n = 0;
    n = sig_diag_put(b, n, "[HLFATAL]");
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
#if G_GPC_HASH_SHIFT == 2
    if (c && host_range_mapped((uintptr_t)G_PC(c), 4))
        n = sig_diag_put_hex(b, n, " insn=", *(const uint32_t *)(uintptr_t)G_PC(c));
#endif
    ucontext_t *u = (ucontext_t *)ucv;
    /* HL_HOST_UC_PC (native_context.h) is host-neutral: pc on AArch64, rip on x86-64. */
    uint64_t hpc = u ? (uint64_t)HL_HOST_UC_PC(u) : 0;
    uint64_t hgpc = 0, hoff = 0;
    uint32_t hinsn = 0;
    extern int jit_hostpc_lookup(uint64_t hpc, uint64_t *gpc, uint64_t *off, uint32_t *insn);
    if (jit_hostpc_lookup(hpc, &hgpc, &hoff, &hinsn)) {
        n = sig_diag_put_hex(b, n, " hpc=", hpc);
        n = sig_diag_put_hex(b, n, " hblk=", hgpc);
        n = sig_diag_put_hex(b, n, " hoff=", hoff);
        n = sig_diag_put_hex(b, n, " hinsn=", hinsn);
    }
    b[n++] = '\n';
    sig_diag_write(b, (size_t)n);
}

static void sig_diag_sync_reraise(int sig, int ls, siginfo_t *si, void *ucv) {
    ucontext_t *u = (ucontext_t *)ucv;
    /* HL_HOST_UC_PC resolves OS and CPU together; a Darwin-shaped `__ss.__rip` under a bare __x86_64__
     * guard does not compile against a Linux ucontext_t. */
    uint64_t hpc = u ? (uint64_t)HL_HOST_UC_PC(u) : 0;
    uint64_t hgpc = 0, hoff = 0;
    uint32_t hinsn = 0;
    extern int jit_hostpc_lookup(uint64_t hpc, uint64_t *gpc, uint64_t *off, uint32_t *insn);
    int hit = jit_hostpc_lookup(hpc, &hgpc, &hoff, &hinsn);
    struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
    char b[512];
    int n = 0;
    n = sig_diag_put(b, n, "[HLSYNC]");
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
    sig_diag_write(b, (size_t)n);
}

static void sig_diag_raise_default(struct cpu *c, int sig) {
    // An engine-internal diagnostic for a guest taking a fatal-default signal. It must NEVER reach the
    // guest's own stderr fd, so route it through the engine's tagged logging facility (HL_LOG_TAG_SIGNAL)
    // exactly like every other engine diagnostic -- gated on the HL_LOG selector and compiled out entirely
    // in a production (HL_ENABLE_LOGGING=0) build. A raw write(STDERR_FILENO) here leaked "[HLRAISE] ..."
    // into the guest's captured stderr on any uncaught fatal signal (e.g. `kill -TERM $$`).
    HL_LOGF(
        &g_jit_log, HL_LOG_TAG_SIGNAL,
        "raise-default pid=%#llx cpid=%#llx tid=%#llx sig=%#llx pc=%#llx sp=%#llx lr=%#llx handler=%#llx mask=%#llx",
        (unsigned long long)getpid(), (unsigned long long)container_pid(),
        (unsigned long long)(c ? (uint64_t)cpu_tid(c) : 0), (unsigned long long)sig,
        (unsigned long long)(c ? G_PC(c) : 0), (unsigned long long)(c ? G_SP(c) : 0),
#if G_GPC_HASH_SHIFT == 2
        (unsigned long long)(c ? c->x[30] : 0),
#else
        0ull,
#endif
        (unsigned long long)((sig >= 1 && sig <= 64) ? g_sigact[sig].handler : 0),
        (unsigned long long)(c ? c->sigmask : 0));
}

// a GENUINE synchronous CPU fault (SIGSEGV/SIGBUS/...) taken in translated code for which the guest
// installed NO handler. Such a fault is fatal and cannot be masked or ignored (a stack overflow into the
// guard gap, a wild pointer, a NULL deref). Terminate the guest process the SAME way hl terminates any
// fatal-default signal, so the exit status crosses hl's fork faithfully: the container init ends with
// 128+signo; a non-init guest records the intended Linux termination signal (sigexit_record) then exits
// via the normal c->exited path so its parent's wait4/waitid reconstructs WIFSIGNALED/WTERMSIG=signo. A
// raw host raise() cannot carry the signo across hl's fork and, from a MAP_JIT thread, degrades to a plain
// exit(255) (the parent then wrongly sees WIFEXITED, not WIFSIGNALED). Called by the per-arch SIGSEGV/SIGBUS
// guard AFTER deliver_guest_fault (the guest-handler path) declines. Returns 1 iff this was a genuine
// in-translated-code guest fault (caller stops); 0 for an engine fault / external async signal, so the
// caller re-raises the real crash unchanged.
static int deliver_guest_fatal_fault(int hostsig, siginfo_t *si, void *ucv) {
    int sig = sig_m2l(hostsig);
    if (sig < 1 || sig > 64 || !ucv) return 0;
    // Apply the same async-signal-safe host-to-guest classification as the guest-handler path above.
    if (hostsig == SIGBUS &&
#if defined(__APPLE__)
        !g_in_service && si && !hl_linux_bus_hit((uint64_t)si->si_addr, 1)
#else
        HOST_SIGNAL_HAS_FAULT_ADDRESS(si) && si->si_addr &&
        (gna_hit((uint64_t)si->si_addr, 1) || !host_addr_mapped((uintptr_t)si->si_addr))
#endif
    )
        sig = 11;
    if (g_sigact[sig].handler > 1) return 0; // a guest handler exists -> not ours (deliver_guest_fault owns it)
    struct cpu *c = (struct cpu *)pthread_getspecific(g_cpu_key);
    if (!c) return 0;
    if (!sigframe_capture_fault(c, ucv)) return 0; // host PC not in translated code -> engine/async: re-raise
    sig_diag_fatal_fault(sig, hostsig, si, c, ucv);
    // A genuine, fatal, unmaskable guest fault. Terminate the guest process HERE (async-signal-safe _exit),
    // not by resuming the dispatcher: the guest state is captured mid-fault (e.g. SP overrun into the guard),
    // so re-entering the code cache would run off into garbage. A non-init guest records its Linux
    // termination signo so the parent's wait4/waitid reconstructs WIFSIGNALED/WTERMSIG=sig (proc.c case 260);
    // the container init just exits 128+signo (what `docker run` reports for a crash). This is hl's standard
    // fatal-signal relay -- the same mechanism as a fatal-default signal in maybe_deliver_signal.
    guest_group_fatal(c, sig);
}

// Linux mmap flags -> macOS.
static int mmap_flags(int lf) {
    int f = 0;
    if (lf & 0x01) f |= MAP_SHARED;
    if (lf & 0x02) f |= MAP_PRIVATE;
    if (lf & 0x10) f |= MAP_FIXED;
    if (lf & 0x20) f |= MAP_ANON;
#if defined(__linux__)
    // On a Linux host the guest's Linux MAP_* bits ARE the host's bits, so the placement/behavior flags
    // above the type bits can be forwarded verbatim and enforced by the kernel itself instead of being
    // silently dropped (which turned MAP_FIXED_NOREPLACE into a plain hint that CLOBBERED an existing
    // mapping, made MAP_HUGETLB fake-succeed as ordinary pages, and ignored MAP_POPULATE/LOCKED/NORESERVE).
    // Forward the exact bits the kernel would honor; the type bits (0x01/0x02/0x10/0x20) are already set,
    // and MAP_32BIT (0x40) is x86-guest-specific and meaningless on this aarch64 host, so both are excluded.
    //   GROWSDOWN 0x100, LOCKED 0x2000, NORESERVE 0x4000, POPULATE 0x8000, NONBLOCK 0x10000,
    //   STACK 0x20000, HUGETLB 0x40000, FIXED_NOREPLACE 0x100000, MAP_HUGE_* size (0x3f << 26).
    f |= lf & (0x100 | 0x2000 | 0x4000 | 0x8000 | 0x10000 | 0x20000 | 0x40000 | 0x100000 | (0x3f << 26));
#endif
    return f;
}
