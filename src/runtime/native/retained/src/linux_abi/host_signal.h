#ifndef HL_LINUX_ABI_HOST_SIGNAL_H
#define HL_LINUX_ABI_HOST_SIGNAL_H

/*
 * <signal.h> for this layer -- the POSIX half of it, which the mingw CRT does
 * not have.  Same construction and the same REAL/SHAPE/REFUSAL labelling as
 * host_mman.h and host_poll.h.
 *
 * This one is FORCE-INCLUDED at the top of the Windows guest-target unity TU
 * (`-include`), not reached through an #include in some .c file, because the
 * vocabulary it supplies is needed before the first system header that names it
 * and because <signal.h> on this host exists and is merely INCOMPLETE -- there
 * is no missing file to intercept.  Being force-included is also why it pulls
 * its own <signal.h>/<errno.h>/<stdint.h>: nothing has run before it.
 *
 * WHAT THE mingw CRT ALREADY HAS, and is therefore NOT redefined below:
 *
 *   sig_atomic_t, signal(), raise(), SIG_DFL/SIG_IGN/SIG_ERR (plus the
 *   Windows-only SIG_GET/SIG_SGE/SIG_ACK), NSIG (23), and the numbers
 *   SIGINT 2, SIGILL 4, SIGFPE 8, SIGSEGV 11, SIGTERM 15, SIGBREAK 21,
 *   SIGABRT 22.
 *
 *   Five of those seven numbers are already the Linux ones, which is descent
 *   rather than design: the CRT set and the Linux set share a 1980s ancestor.
 *   The two that are not are worth naming because they are traps:
 *
 *     - SIGABRT is 22 here and 6 on Linux.  22 is Linux's SIGTTOU.  Nothing in
 *       this layer names SIGABRT -- signal.c spells the guest's abort signal as
 *       the literal 6 in sig_coredumps() -- so the mismatch is inert today, and
 *       redefining it would break the CRT's own abort().  It is left alone,
 *       loudly.
 *     - SIGBREAK is 21, which is Linux's SIGTTIN.  Same reasoning: distinct
 *       spellings, and no code here says SIGBREAK.
 *
 *   NSIG is 23 and Linux's _NSIG is 65.  NSIG is not redefined either; this
 *   layer sizes its own tables with literal 64/65 (g_sigact[65], sig_is_rt()
 *   testing 32..64) and never asks the host how many signals exist.
 *
 * WHY THE CALLS ARE REFUSALS.  Windows has no signal mask, no per-signal
 * disposition table, and no asynchronous signal delivery to a thread.  The
 * mechanisms it does have are not smaller versions of those, they are different
 * mechanisms:
 *
 *   - A FAULT (the SIGSEGV/SIGBUS/SIGILL/SIGFPE/SIGTRAP class, which is what
 *     this layer's host handlers are overwhelmingly for) arrives as a
 *     structured exception.  The engine's interception point on this host is a
 *     process-wide VECTORED exception handler receiving an EXCEPTION_POINTERS,
 *     whose ContextRecord is edited in place and resumed with
 *     EXCEPTION_CONTINUE_EXECUTION -- the analogue of editing a ucontext_t and
 *     returning from a POSIX handler, but registered ONCE for the process
 *     instead of once per signal number, with no mask, no altstack, no
 *     re-raise, and no saved previous disposition to restore.  A sigaction()
 *     that "succeeded" would record a disposition nothing consults, and the
 *     fault would still go to the VEH.
 *
 *   - An ASYNC signal (kill/tgkill/pthread_kill of SIGCHLD, SIGURG, a realtime
 *     signal) has no host equivalent at all.  The nearest primitives are
 *     QueueUserAPC and an event object, and neither interrupts a thread parked
 *     in a blocking call the way a signal does.
 *
 *   - GUEST signal delivery -- the guest's own rt_sigaction table, its
 *     per-thread mask, the rt_sigframe built on the guest stack -- is a
 *     separate, UNBUILT subsystem on this host.  It is emulated in this layer
 *     (g_sigact, build_signal_frame) and bottoms out in the host only for the
 *     two things the host must do: notice the fault, and wake a blocked thread.
 *
 * So a refusal here is honest and a fake success is not.  Concretely, the
 * failure a fake success buys is: a guest calls rt_sigaction to install a
 * SIGSEGV handler, this layer forwards the disposition to the host with
 * sigaction(), the host answers 0, the guest faults, and nothing runs -- the
 * process dies with the handler apparently installed.  That is strictly worse
 * to debug than an ENOSYS at install time.  (Same argument host_poll.h makes
 * about returning 0 from poll: the SHAPE of the lie matters, not just that
 * there is one.)
 *
 * The one family that is REAL is the sigset_t manipulators.  sigemptyset and
 * friends are pure bit arithmetic on a value this header defines; no kernel is
 * involved on any host, so there is nothing to refuse.  They are correct here
 * in exactly the sense they are correct on Linux.  What the resulting set is
 * then USED for (sigprocmask) is the part that refuses.
 */

#if !defined(_WIN32)

#include <signal.h>

#else /* Windows */

#include "../host/process.h" /* the backend's process table, for kill() below */

#include <errno.h>
#include <setjmp.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>
#include <sys/time.h>
#include <sys/types.h>

/*
 * FIRST, before anything else: winpthreads' <pthread_signal.h> -- which the
 * CRT's own <signal.h> includes, so it has just run -- contains
 *
 *     #define pthread_sigmask(H, S1, S2) 0
 *
 * That is precisely the fake success this seam exists to forbid: every
 * pthread_sigmask call site in this layer would silently become the constant 0,
 * with its arguments not even evaluated, and the caller would believe the
 * thread's mask changed.  Undefine it here, once, and answer honestly below.
 * One #undef covers the whole TU: pthread_signal.h has its own include guard,
 * so nothing re-establishes the macro when <pthread.h> arrives later.
 */
#undef pthread_sigmask

/* ---- SHAPE: the Linux signal numbers the CRT does not define. ------------
 * Linux/x86-64 values, which are also the Linux/aarch64 values -- the whole
 * 1..31 block is architecture-independent on Linux.  These are guest signal
 * numbers as often as they are host ones (sig_l2m/sig_m2l translate by identity
 * on a Linux host), so any other numbering would be wrong twice. */
#define SIGHUP 1
#define SIGQUIT 3
#define SIGTRAP 5
#define SIGIOT 6
#define SIGBUS 7
#define SIGKILL 9
#define SIGUSR1 10
#define SIGUSR2 12
#define SIGPIPE 13
#define SIGALRM 14
#define SIGSTKFLT 16
#define SIGCHLD 17
#define SIGCLD SIGCHLD
#define SIGCONT 18
#define SIGSTOP 19
#define SIGTSTP 20
#define SIGTTIN 21
#define SIGTTOU 22
#define SIGURG 23
#define SIGXCPU 24
#define SIGXFSZ 25
#define SIGVTALRM 26
#define SIGPROF 27
#define SIGWINCH 28
#define SIGIO 29
#define SIGPOLL SIGIO
#define SIGPWR 30
#define SIGSYS 31
#define SIGUNUSED 31

/* SHAPE.  The realtime range, Linux values.  Spelled as plain integers rather
 * than glibc's "first free after the library's private ones" runtime call,
 * because this layer's own tables already hard-code the same range
 * (sig_is_rt() tests 32..64, g_sigact has 65 slots).
 *
 * NOTE the collision, deliberately not hidden: src/host/native_compat.h's
 * Windows arm defines the engine's two private control signals as SIGEMT 32 and
 * SIGINFO 33, chosen to sit ABOVE the CRT's NSIG so that every CRT/winpthreads
 * call carrying one fails with EINVAL instead of landing on a real signal.
 * Those numbers are SIGRTMIN and SIGRTMIN+1 here.  The alias is harmless today
 * only because this host delivers no signals at all: pthread_kill(t, 32) is
 * rejected by winpthreads, so the two meanings can never both be live.  When
 * signal delivery is actually built for Windows, one of the two numbering
 * schemes has to move, and this is the note that says so. */
#define SIGRTMIN 32
#define SIGRTMAX 64

/* ---- SHAPE: sigaction flags.  Linux/x86-64 values. ---------------------- */
#define SA_NOCLDSTOP 0x00000001
#define SA_NOCLDWAIT 0x00000002
#define SA_SIGINFO 0x00000004
#define SA_RESTORER 0x04000000
#define SA_ONSTACK 0x08000000
#define SA_RESTART 0x10000000
#define SA_NODEFER 0x40000000
#define SA_NOMASK SA_NODEFER
#define SA_RESETHAND 0x80000000
#define SA_ONESHOT SA_RESETHAND

/* ---- SHAPE: sigprocmask/pthread_sigmask "how".  Linux values. ----------- */
#define SIG_BLOCK 0
#define SIG_UNBLOCK 1
#define SIG_SETMASK 2

/* ---- SHAPE: si_code values.  Linux values. ------------------------------
 * The negative ones are the "something sent this" codes and the positive ones
 * the "the kernel faulted" codes; signal.c's HOST_SIGNAL_HAS_FAULT_ADDRESS
 * keys on exactly that sign convention on Linux, so the signs matter as much as
 * the magnitudes. */
#define SI_USER 0
#define SI_KERNEL 0x80
#define SI_QUEUE (-1)
#define SI_TIMER (-2)
#define SI_MESGQ (-3)
#define SI_ASYNCIO (-4)
#define SI_SIGIO (-5)
#define SI_TKILL (-6)

#define CLD_EXITED 1
#define CLD_KILLED 2
#define CLD_DUMPED 3
#define CLD_TRAPPED 4
#define CLD_STOPPED 5
#define CLD_CONTINUED 6

#define SEGV_MAPERR 1
#define SEGV_ACCERR 2

#define BUS_ADRALN 1
#define BUS_ADRERR 2
#define BUS_OBJERR 3

#define ILL_ILLOPC 1
#define ILL_ILLOPN 2
#define ILL_ILLADR 3
#define ILL_ILLTRP 4
#define ILL_PRVOPC 5
#define ILL_PRVREG 6
#define ILL_COPROC 7
#define ILL_BADSTK 8

#define FPE_INTDIV 1
#define FPE_INTOVF 2
#define FPE_FLTDIV 3
#define FPE_FLTOVF 4
#define FPE_FLTUND 5
#define FPE_FLTRES 6
#define FPE_FLTINV 7
#define FPE_FLTSUB 8

#define TRAP_BRKPT 1
#define TRAP_TRACE 2

/* ---- SHAPE: alternate-stack words.  Linux values. ----------------------- */
#define SS_ONSTACK 1
#define SS_DISABLE 2
#define MINSIGSTKSZ 2048
#define SIGSTKSZ 8192

/*
 * SHAPE.  sigset_t, Linux's 1024-bit shape.
 *
 * Spelled with `unsigned long long` and not glibc's `unsigned long` on purpose:
 * a Windows long is 32 bits, so the literal glibc spelling would produce a
 * 512-bit, 64-byte set here.  The member keeps glibc's `__val` name so that
 * anything poking at the representation reads the same on both.
 *
 * 1024 bits is far more than the 64 signals anything uses, and that is the
 * point -- it is the size a guest's rt_sigprocmask sigsetsize argument is
 * measured against elsewhere in this layer, and keeping ONE number for "how big
 * is a sigset" avoids acquiring a second one.
 */
typedef struct {
    unsigned long long __val[16];
} sigset_t;

/* SHAPE.  Linux's sigval, both spellings. */
union sigval {
    int sival_int;
    void *sival_ptr;
};

/*
 * SHAPE.  siginfo_t.
 *
 * Laid out to match Linux's OFFSETS, not merely its field names, because the
 * aliasing is load-bearing for readers of this type: signal.c's own comment on
 * struct sigq_ent records that si_value and si_status share offset 24 (the
 * _sigchld and _rt arms of the kernel union), and code that reads one after
 * writing the other depends on it.  So the payload is an anonymous union of the
 * arms this layer actually touches, each field appearing exactly once:
 *
 *   16: si_pid / si_addr / si_band      20: si_uid
 *   24: si_status | si_value / si_fd
 *
 * Only arms with a live reader are spelled out (kill+rt+sigchld, sigfault,
 * sigpoll).  _sigsys (si_call_addr/si_syscall/si_arch) and the timer arm
 * (si_tid/si_overrun) are omitted: nothing here reads a HOST siginfo for those,
 * and the guest-visible ones are built by hand into the guest's frame rather
 * than copied out of a host struct.  Adding a field nobody reads would imply a
 * host fills it, and on this host nothing produces a siginfo_t at all.
 */
typedef struct {
    int si_signo;
    int si_errno;
    int si_code;
    int __si_pad0;

    union {
        struct {
            int si_pid;
            unsigned int si_uid;

            union {
                int si_status;
                union sigval si_value;
            };
        };

        struct {
            void *si_addr;
            short si_addr_lsb;
        };

        struct {
            /* Linux's si_band is `long int`, which is 64 bits there and 32 here;
             * spelled `long long` for the same reason sigset_t is, so si_fd
             * still lands at 24 rather than sliding to 20. */
            long long si_band;
            int si_fd;
        };

        char __si_pad[112];
    };
} siginfo_t;

/* SHAPE.  Linux spells the plain handler type __sighandler_t; the CRT's own
 * __p_sig_fn_t is the identical type (void (*)(int), cdecl being the default
 * on this target), so SIG_DFL/SIG_IGN assign into either without a cast. */
typedef void (*__sighandler_t)(int);
typedef __sighandler_t sighandler_t;

/*
 * SHAPE.  struct sigaction, with Linux's field names.
 *
 * sa_handler and sa_sigaction are an ANONYMOUS union rather than glibc's named
 * union plus two object-like #defines: a bare `#define sa_handler ...` at file
 * scope in a FORCE-INCLUDED header would rewrite every identifier of that name
 * anywhere in the translation unit, and a unity TU this size is exactly where
 * that goes wrong.
 *
 * Field ORDER follows glibc's userspace struct (handler, mask, flags,
 * restorer), which differs from the kernel's (handler, flags, restorer, mask).
 * Nothing on this host serializes or memcpy's this struct -- every use is
 * field-by-field on a local -- so the order is unobservable here; the guest's
 * own kernel-shaped sigaction is a separate, hand-marshalled layout in
 * syscall/signal.c and never travels through this type.
 */
struct sigaction {
    union {
        __sighandler_t sa_handler;
        void (*sa_sigaction)(int, siginfo_t *, void *);
    };

    sigset_t sa_mask;
    int sa_flags;
    void (*sa_restorer)(void);
};

/* SHAPE.  Linux's stack_t. */
typedef struct {
    void *ss_sp;
    int ss_flags;
    size_t ss_size;
} stack_t;

/* ---- REAL: the sigset_t manipulators. -----------------------------------
 * Pure bit arithmetic over the struct defined above; there is no host object to
 * ask, on any host.  Signal n occupies bit (n-1), which is Linux's convention.
 * Range-checked the way glibc's are (EINVAL outside the representable range)
 * rather than silently scribbling past the array. */
static inline int hl_linux_sigset_bit_ok(int signo) {
    return signo >= 1 && signo <= 1024;
}

static inline int sigemptyset(sigset_t *set) {
    if (set == NULL) {
        errno = EINVAL;
        return -1;
    }
    memset(set, 0, sizeof *set);
    return 0;
}

static inline int sigfillset(sigset_t *set) {
    if (set == NULL) {
        errno = EINVAL;
        return -1;
    }
    memset(set, 0xff, sizeof *set);
    return 0;
}

static inline int sigaddset(sigset_t *set, int signo) {
    if (set == NULL || !hl_linux_sigset_bit_ok(signo)) {
        errno = EINVAL;
        return -1;
    }
    set->__val[(unsigned)(signo - 1) / 64] |= 1ull << ((unsigned)(signo - 1) % 64);
    return 0;
}

static inline int sigdelset(sigset_t *set, int signo) {
    if (set == NULL || !hl_linux_sigset_bit_ok(signo)) {
        errno = EINVAL;
        return -1;
    }
    set->__val[(unsigned)(signo - 1) / 64] &= ~(1ull << ((unsigned)(signo - 1) % 64));
    return 0;
}

static inline int sigismember(const sigset_t *set, int signo) {
    if (set == NULL || !hl_linux_sigset_bit_ok(signo)) {
        errno = EINVAL;
        return -1;
    }
    return (int)((set->__val[(unsigned)(signo - 1) / 64] >> ((unsigned)(signo - 1) % 64)) & 1u);
}

/* ---- REAL: the non-local jump pair. -------------------------------------
 * sigsetjmp/siglongjmp differ from setjmp/longjmp in exactly one respect -- the
 * savemask argument, which asks for the SIGNAL MASK to be saved and restored
 * alongside the jump.  This host has no signal mask, so there is nothing for
 * that argument to select and the two pairs are the same operation.  That makes
 * these REAL rather than refusals: the jump itself is what every caller wants
 * (thread.c's host-read-memory probe, the two interpreters' fault pads), and it
 * works here exactly as it does on Linux.
 *
 * Every caller in this tree already passes savemask == 0, so today not even the
 * difference is exercised. */
typedef jmp_buf sigjmp_buf;
#define sigsetjmp(env, savemask) setjmp(env)
#define siglongjmp(env, value) longjmp(env, value)

/* ---- REFUSAL: everything needing a host disposition, mask, or delivery. -- */

/* REFUSAL.  See the header note: the disposition table this would write does
 * not exist, and the fault path is a process-wide VEH that consults no
 * per-signal record.  A quiet 0 loses a guest's handler installation silently,
 * which is the worst outcome on the menu. */
static inline int sigaction(int signo, const struct sigaction *action, struct sigaction *previous) {
    (void)signo;
    (void)action;
    (void)previous;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL.  There is no blocked-signal set on this host to read or write.
 * Reporting success with an unchanged *previous would make every block/restore
 * bracket in this layer -- fs.c's SIGTTOU fence around a tty ioctl, the
 * check/sleep race close in syscall/signal.c -- look like it took effect while
 * the window it protects stayed wide open. */
static inline int sigprocmask(int how, const sigset_t *set, sigset_t *previous) {
    (void)how;
    (void)set;
    (void)previous;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL.  Same absence, per thread.  This replaces winpthreads' macro (see
 * the #undef at the top of this arm), which answered a literal 0. */
static inline int pthread_sigmask(int how, const sigset_t *set, sigset_t *previous) {
    (void)how;
    (void)set;
    (void)previous;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL.  Pending-set query; there is no pending set.  "Empty" is a specific
 * claim -- nothing is waiting -- that this host cannot make. */
static inline int sigpending(sigset_t *set) {
    (void)set;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL.  sigsuspend is "atomically install this mask and sleep until a
 * signal arrives".  Neither half exists.  Note what the two plausible fakes
 * cost: returning -1/EINTR immediately turns syscall/signal.c's rt_sigsuspend
 * and pause() loops into busy spins, and blocking forever hangs the guest
 * thread with nothing able to wake it.  ENOSYS is the only answer that is
 * neither. */
static inline int sigsuspend(const sigset_t *mask) {
    (void)mask;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL.  A timed wait for a signal that cannot be delivered.  Returning 0
 * ("timed out") would turn rt_sigtimedwait into a guest-visible timer that
 * never fires; returning a signal number would invent a delivery. */
static inline int sigtimedwait(const sigset_t *set, siginfo_t *info, const struct timespec *timeout) {
    (void)set;
    (void)info;
    (void)timeout;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL.  Queue a signal with a payload -- no queue, no delivery. */
static inline int sigqueue(pid_t pid, int signo, const union sigval value) {
    (void)pid;
    (void)signo;
    (void)value;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL.  There is no alternate signal stack, because there is no handler
 * frame to run on one: a VEH runs on the faulting thread's own stack, which is
 * precisely the case an altstack exists to survive.  (So a guard-page overflow
 * has no recovery path on this host yet -- worth knowing, and not something a
 * success here would fix.)
 *
 * Both callers in thread.c already handle the failure correctly:
 * install_host_sigaltstack releases the region it just mapped and returns, and
 * uninstall_host_sigaltstack returns with the handle retained rather than
 * forgetting provider-owned memory. */
static inline int sigaltstack(const stack_t *stack, stack_t *previous) {
    (void)stack;
    (void)previous;
    errno = ENOSYS;
    return -1;
}

/* PARTLY REAL.  kill(2): send a signal to a process.  The host process table
 * this used to lack now exists -- the clone that implements guest fork(2) fills
 * it -- so the two halves of kill can be separated instead of refused together.
 *
 *   Signal 0 is REAL.  It sends nothing; it asks whether a pid names a process
 *   this caller could signal, and that question Windows can answer.  It is also
 *   the half that was actually being used: the container registry's membership
 *   check, its /proc enumeration and its stale-marker pruning are all kill(p, 0)
 *   probes, and under the whole refusal every one of them read "dead" for a
 *   process that was alive.
 *
 *   SIGKILL is REAL, because TerminateProcess IS SIGKILL: immediate,
 *   unmaskable, no handler, and the exit code it mints is decoded back into
 *   WIFSIGNALED(SIGKILL) by the same reap that decodes every other death.
 *
 *   Every other signal is still REFUSED.  Nothing on this host delivers a
 *   catchable signal to another process, and terminating a process that may have
 *   installed a handler would report a death the guest asked to be able to
 *   prevent.  The earlier note's objection -- that a caller cannot tell which
 *   half it got -- is answered by the split being on the SIGNAL NUMBER, which
 *   the caller chose: a guest asking for SIGKILL always gets a kill, a guest
 *   asking for SIGUSR1 always gets ENOSYS, and neither ever gets the other. */
static inline int kill(pid_t pid, int signo) {
    return hl_host_windows_kill(pid, signo);
}

static inline int killpg(pid_t pgrp, int signo) {
    (void)pgrp;
    (void)signo;
    errno = ENOSYS;
    return -1;
}

static inline int tgkill(pid_t tgid, pid_t tid, int signo) {
    (void)tgid;
    (void)tid;
    (void)signo;
    errno = ENOSYS;
    return -1;
}

/*
 * pthread_kill is deliberately NOT defined here.  winpthreads declares and
 * exports a real one, and it already does the honest thing: it validates the
 * signal number against the CRT's NSIG and fails with EINVAL for anything
 * outside it -- which is every number this engine sends, the control signals
 * being 32 and 33 precisely so they land there.  Shadowing an existing extern
 * declaration with a static inline is also a hard error, not a preference.
 *
 * strsignal/psignal are likewise absent.  No call site in this tree uses them,
 * and neither has an honest refusal form: both yield (or print) a string, so
 * "failure" would have to be a NULL that a caller then hands to %s.
 *
 * sigwait/sigwaitinfo are absent for a different reason: nothing calls them,
 * and <pthread.h> wraps both names in cancellation-point macros under
 * __WINPTHREAD_ENABLE_WRAP_API.  Defining them here would put this seam inside
 * that rewrite for no caller's benefit.
 */

/* ---- SHAPE + REFUSAL: the interval timers. ------------------------------
 * setitimer/getitimer live in <sys/time.h> on a POSIX host, which this host has
 * but without them.  They sit here rather than in a seam of their own because
 * what an interval timer DOES is deliver SIGALRM/SIGVTALRM/SIGPROF, so they are
 * the same absence as everything above: Windows has waitable timers and timer
 * queues, but nothing that raises a signal at a thread.
 *
 * Guarded on ITIMER_REAL so that if a <sys/time.h> replacement ever grows them,
 * this arm steps aside instead of colliding. */
#ifndef ITIMER_REAL
#define ITIMER_REAL 0
#define ITIMER_VIRTUAL 1
#define ITIMER_PROF 2

struct itimerval {
    struct timeval it_interval;
    struct timeval it_value;
};

static inline int setitimer(int which, const struct itimerval *value, struct itimerval *previous) {
    (void)which;
    (void)value;
    (void)previous;
    errno = ENOSYS;
    return -1;
}

static inline int getitimer(int which, struct itimerval *value) {
    (void)which;
    (void)value;
    errno = ENOSYS;
    return -1;
}
#endif /* ITIMER_REAL */

#endif /* _WIN32 */

#endif
