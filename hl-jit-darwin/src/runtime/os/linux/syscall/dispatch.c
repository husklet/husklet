// brk arena
static uint64_t brk_lo, brk_cur, brk_hi;
// W3D fork-server prewarm/worker: when set, the guest's exit_group UNWINDS run_guest (sets c->exited
// + c->exit_code) instead of _exit()ing, so the resident ddjitd parent survives pre-translating a
// binary into the COW arena and a worker can report its exit code before dying. 0 on every normal
// (standalone) run -> exit_group behaves exactly as before.
int g_noexit;
// W6A item 3: set the first time a guest requests a PROT_EXEC (RWX) anonymous mapping -- i.e. a
// guest with its own in-process JIT (JVM/V8/LuaJIT/.NET/PyPy). Normal guests never set it, so the
// SMC write-fault invalidation path (frontend/x86_64) stays completely inert for the whole existing
// test matrix (g_rwx_guest==0 -> smc_protect()/smc_on_write() are no-ops -> bit-exact).
int g_rwx_guest;
// dd/runtime/os/linux -- service(): the Linux syscall layer (the "kernel" the guest talks to).
// Dispatches the guest syscall number to the host, translating the ABI (errno, struct layouts, flags,
// fd semantics); every path argument is resolved through the container VFS jail. One sorted switch,
// grouped by category. See docs/SYSCALLS.md for the per-syscall table.

#include <sys/ipc.h>
#include <sys/shm.h>
#include <sys/sem.h>
#include <sys/msg.h>
#include <dirent.h>
#include <stdlib.h>
#include <sys/times.h> // times(2): CPU accounting (struct tms is layout-compatible with Linux)
#include <sys/mount.h> // host struct statfs -> translated to the Linux statfs layout
// seccomp: the classic-BPF interpreter + per-thread filter storage + the service() entry gate. Included
// here (before the fs/proc/rare family includes below) so proc.c's PR_SET_SECCOMP and rare.c's seccomp(2)
// handlers can call seccomp_install_filter/seccomp_set_strict, and so service() can call seccomp_gate.
#include "../seccomp.c"
// macOS renamex_np/renameatx_np flags (Linux renameat2 flags map onto these)
#ifndef RENAME_SWAP
#define RENAME_SWAP 0x00000002 // atomic swap  <- Linux RENAME_EXCHANGE(2)
#endif
#ifndef RENAME_EXCL
#define RENAME_EXCL 0x00000004 // fail if dst exists <- Linux RENAME_NOREPLACE(1)
#endif
// ================= ptrace(2) — in-dd tracer/tracee coordination ========================
// dd runs each guest PROCESS as its own host process (fork(2) is a real host fork; see proc.c case 220),
// so a guest tracer ptracing a guest tracee is TWO host processes. We CANNOT proxy to the host macOS
// ptrace: macOS ptrace has no Linux semantics and cannot see the tracee's GUEST register file (which
// lives in the tracee's own `struct cpu`). Instead we emulate the ptrace relationship *between* the two
// guest processes over a shared-memory arena (MAP_SHARED|ANON, mmap'd ONCE at engine_global_init BEFORE
// any guest fork, so every descendant guest process inherits the same physical pages). Keyed on guest
// pids (dd already maps guest<->host pids via g_init_hostpid/container_pid). The tracee publishes its
// marshalled GUEST registers into its slot whenever it enters a ptrace-stop; the tracer's ptrace()
// calls read/steer that slot; PEEK/POKE of tracee memory are serviced by the (stopped) tracee itself
// over a request/response channel in the slot (the tracer's own address space holds a COW copy, not the
// tracee's, so it cannot read tracee memory directly). See os/linux/syscall/ptrace.c for the whole
// design + the staged-work enumeration. Inert (one relaxed load) whenever no process is being traced.
// Declared here (before the family includes) so mem.c/proc.c/rare.c can call the hooks; the arena struct
// is defined here because service() reads g_pt->nactive on the hot path.
#define PT_MAXLINK 128
#define PT_MEMBUF 1024
#define PT_PVM_LOCAL ((long)0x7fffffffffffffffLL) // ptrace_pvm: "remote is self -> same-space memcpy"

struct pt_link {
    volatile int used, attached, seized;
    volatile int tracer_pid, tracee_pid; // guest pids (init == 1)
    volatile uint64_t options;           // PTRACE_SETOPTIONS bits
    volatile int stopstate;              // 0 running, 1 stopped
    volatile int stopkind;               // PTS_* (see ptrace.c)
    volatile int stopsig;                // reported signal (signal/group stops)
    volatile int event;                  // PTRACE_EVENT_* (0 = none)
    volatile unsigned long eventmsg;     // PTRACE_GETEVENTMSG
    volatile int waitstatus;             // Linux-encoded status the tracer's wait sees
    volatile int reported;               // tracer has consumed this stop via wait
    volatile int syscall_mode;           // stop at every syscall (armed by PTRACE_SYSCALL)
    volatile int pending_attach_stop;    // ATTACH/SEIZE/INTERRUPT -> stop at next syscall boundary
    volatile int cmd, cmd_sig;           // pending resume command + signal to inject
    volatile int inject_pass;            // a tracer-injected signal to deliver ONCE without re-trapping
    volatile unsigned cmd_seq, ack_seq;  // tracer bumps cmd_seq; tracee acks with ack_seq
    volatile int arch;                   // 0 x86_64, 1 aarch64
    volatile int reglen;                 // marshalled user_regs_struct byte length
    volatile uint64_t regs[40];          // marshalled register image (published by tracee at each stop)
    volatile int regs_dirty;             // tracer SETREGS -> tracee reloads on resume
    volatile uint64_t entry_nr;          // syscall nr captured at entry (orig_rax at exit-stop, x86)
    volatile uint8_t siginfo[128];       // last stop's siginfo (PTRACE_GETSIGINFO)
    // memory request/response (serviced by the stopped tracee against its OWN guest address space)
    volatile int mem_dir; // 0 none, 1 read, 2 write
    volatile uint64_t mem_addr, mem_len;
    volatile unsigned mem_seq, mem_ack;
    volatile int mem_err;
    volatile uint8_t mem_buf[PT_MEMBUF];
};

struct pt_arena {
    volatile int nactive;  // # of live tracee links (hot-path gate)
    volatile uint64_t gen; // bumped on any link table change (per-proc lookup cache key)
    volatile int lock;     // spinlock for slot alloc/free
    struct pt_link link[PT_MAXLINK];
};
static struct pt_arena *g_pt; // shared arena (NULL until ptrace_arena_init)
static void ptrace_arena_init(void);
static int svc_ptrace(struct cpu *c, uint64_t req, uint64_t pid, uint64_t addr, uint64_t data);
static void ptrace_service_traced(struct cpu *c); // service() hot-path hook when g_pt->nactive > 0
static int ptrace_wait(struct cpu *c, pid_t wpid, int opts, struct rusage *ru, int *status, pid_t *out);
static long ptrace_pvm(struct cpu *c, int is_write, pid_t rpid, const struct iovec *liov, unsigned long ln,
                       const struct iovec *riov, unsigned long rn);
static int ptrace_any_tracee_of_self(void);      // does the caller trace anyone? (wait4 routing)
static int ptrace_wait_active(void);             // is ptrace in use in this session? (wait4 routing gate)
struct sigaction;                                // fwd (signal.h is included by the target before this TU)
static int pt_wait_arm(struct sigaction *saved); // scoped SIGCHLD wake around a blocking wait4
static void pt_wait_disarm(int armed, const struct sigaction *saved);
// the shared per-fd emulation-table teardown (defined in fs.c, included after io.c) -- fwd-declared so
// the dup2/dup3-overwrite path in io.c can shed the destination fd's tables before dup2 reuses the number.
static void fd_reset_emul(int fd);

// ==================== Wall 7 renderer-stall instrumentation (flag-gated) =========================
// Guarded by env DD_WALL7_TRACE=1. When unset this is a single cached-int compare per call site, so
// the whole facility is inert for the normal test matrix. When set it emits one-line, pid-tagged
// traces to stderr for the exact primitives the multi-process-Chrome renderer's Mojo bring-up rides
// on: AF_UNIX socketpair/eventfd creation, epoll_ctl registration, epoll_wait block/wake on those
// fds, and sendmsg/recvmsg with SCM control (fd-passing + synthesized SO_PASSCRED credentials) on
// the primary node channel. Children inherit the launcher's stderr, so a forked renderer's trace
// interleaves with the browser's; correlate by the pid= tag (execve/clone are traced too). The goal
// is to NAME the fd/message/wake the dormant renderer IO thread waits on but never receives.
#include <stdarg.h>
static int g_w7_trace = -1;
static inline int w7_on(void) {
    if (__builtin_expect(g_w7_trace < 0, 0)) g_w7_trace = getenv("DD_WALL7_TRACE") ? 1 : 0;
    return g_w7_trace;
}
__attribute__((format(printf, 1, 2))) static void w7_trace(const char *fmt, ...) {
    if (!w7_on()) return;
    char buf[640];
    int n = snprintf(buf, sizeof buf, "[w7 pid=%d] ", (int)getpid());
    if (n < 0) n = 0;
    if (n > (int)sizeof buf - 2) n = (int)sizeof buf - 2;
    va_list ap;
    va_start(ap, fmt);
    int m = vsnprintf(buf + n, sizeof buf - (size_t)n, fmt, ap);
    va_end(ap);
    if (m > 0) n += (m < (int)sizeof buf - n) ? m : (int)sizeof buf - n - 1;
    if (n < 1 || buf[n - 1] != '\n') buf[n++] = '\n';
    (void)!write(2, buf, (size_t)n);
}

#include "helpers.c"
#include "sysv.c"
#include "mem.c"
#include "signal.c"
#include "time.c"
#include "io.c"
#include "aio.c"
#include "net.c"
#include "event.c"
#include "misc.c"
// --- untrusted-guest isolation seam (subsystem #3: the sentry process-split) --------------------
// The dispatcher's syscall boundary (run_guest -> service(c)) is the entire guest->host authority
// crossing. We interpose a one-branch router so an UNTRUSTED guest's fs/net/proc syscalls can be
// executed in a separate, deny-default-sandboxed SENTRY process instead of in this (JIT-hosting)
// worker. The gate `g_untrusted` and the router `syscall_route()` live in os/linux/sentry.c, which
// the target TU #includes immediately AFTER this file (next to it). When the gate is OFF -- the
// default, and the ENTIRE test matrix -- service() tail-calls service_local(c) (the canonical switch
// below, renamed) through a single statically-predicted-not-taken branch: byte-identical to the
// pre-split engine, no behavior change, no measurable cost. The fork/ring/Seatbelt machinery only
// exists under the gate. This is the ONLY edit to this file.
static int g_untrusted;                   // gate (defined + env-parsed in os/linux/sentry.c)
static void syscall_route(struct cpu *c); // sentry router (defined in os/linux/sentry.c)
static void service_local(struct cpu *c); // fwd: the canonical syscall switch (this file)

// g2h-style redirect for non-PIE ET_EXEC pointer args. A non-PIE links at a fixed low vaddr but is biased
// HIGH by load_elf (__PAGEZERO forbids the low 4 GB); an un-relocated pointer baked at the low link vaddr
// (e.g. a global .rodata string handed to open()/write()) still names the low range, where nothing is
// mapped. The real bytes live at addr+g_nonpie_bias in the high-mapped image, so any *pointer* syscall arg
// that lands in [g_nonpie_lo,g_nonpie_hi) must be rebased before the host syscall dereferences it. Inert
// for PIE/static-PIE (g_nonpie_lo==0, the only state the test matrix ever sees) and for any pointer that is
// already high (stack/heap/bss-above-bias) -> byte-identical there. Apply ONLY to pointer positions.
static inline uint64_t nonpie_p(uint64_t a) {
    return (g_nonpie_lo && a >= g_nonpie_lo && a < g_nonpie_hi) ? a + g_nonpie_bias : a;
}

// Overlay: a metadata/rename syscall (chmod/chown/utimensat/rename) confines to the writable upper via
// jail_at, but a target that still lives only in a read-only lower (the image) is absent from the upper
// -> the op ENOENTs. Copy the target up first (same write-path pattern openat uses) so jail_at finds it.
// No-op when not in overlay mode (g_nlower==0) or when the file is already in the upper. (dirfd,raw) is
// the syscall's AT_FDCWD/dir-fd-relative path; overlay_copyup leaves a genuinely missing path untouched
// so a real bad path still ENOENTs in the upper as before.
static void overlay_copyup_at(int dirfd, const char *raw) {
    if (!g_nlower || !raw) return;
    char gp[4200], host[4300];
    abs_guest(dirfd, raw, gp, sizeof gp);
    overlay_copyup(gp, host, sizeof host);
}

// Overlay: like overlay_copyup_at but RECURSIVE for a lower-only directory -- used by rename(2), where the
// whole subtree must exist in the upper before the move (a plain copyup would materialize an EMPTY dir and
// rename LOSES the contents). A file target falls through to the byte copyup. No-op outside overlay mode.
static void overlay_copyup_at_tree(int dirfd, const char *raw) {
    if (!g_nlower || !raw) return;
    char gp[4200];
    abs_guest(dirfd, raw, gp, sizeof gp);
    overlay_copyup_tree(gp);
}

// Overlay: does a read-only lower still provide `guest` (so it would re-surface once the upper copy is moved
// away)? Mirrors overlay_copyup's lower scan; rootfs-routed paths only (a volume has its own backing dir).
// Used by rename to decide whether the source needs a whiteout. False outside overlay mode (g_nlower==0).
static int overlay_lower_has(const char *guest) {
    if (!g_nlower || !guest || guest[0] != '/') return 0;
    const char *canon;
    size_t clen;
    const char *rel;
    if (jail_pick(guest, &canon, &clen, &rel) != g_root_fd) return 0;
    for (int i = 0; i < g_nlower; i++) {
        char lp[4300];
        struct stat st;
        layer_follow(g_lower[i].canon, g_lower[i].clen, guest, lp, sizeof lp, 1);
        if (lstat(lp, &st) == 0) return 1;
        if (wh_exists(g_lower[i].canon, g_lower[i].clen, guest)) return 0; // hidden below this layer
    }
    return 0;
}

// adjtimex/clock_adjtime read-only query: macOS has no adjtimex, so report an OK-but-unsynchronised
// kernel clock and fill the Linux struct timex the caller passed. Setting the clock (modes != 0) needs
// CAP_SYS_TIME, which the container lacks -> EPERM (mirrors clock_settime). Returns the clock state
// (TIME_OK) or a negative errno. Offsets match the LP64 Linux struct timex.
static int svc_adjtimex(uint8_t *tx) {
    // The struct timex (fields up to tx+88) is a raw guest pointer we read AND write directly; validate the
    // whole 96-byte struct before any deref so a bad/unmapped pointer returns -EFAULT, not an engine fault.
    if (!tx || !host_range_mapped((uintptr_t)tx, 96)) return -EFAULT;
    uint32_t modes = *(uint32_t *)(tx + 0);
    if (modes != 0) return -EPERM; // any clock-adjusting call -> EPERM (no CAP_SYS_TIME)
    struct timeval now;
    gettimeofday(&now, NULL);
    *(int64_t *)(tx + 8) = 0;         // offset (us)
    *(int64_t *)(tx + 16) = 0;        // freq (scaled ppm)
    *(int64_t *)(tx + 24) = 16384;    // maxerror (us)
    *(int64_t *)(tx + 32) = 16384;    // esterror (us)
    *(int32_t *)(tx + 40) = 0x0040;   // status = STA_UNSYNC
    *(int64_t *)(tx + 48) = 2;        // constant
    *(int64_t *)(tx + 56) = 1;        // precision (us)
    *(int64_t *)(tx + 64) = 32768000; // tolerance (default)
    *(int64_t *)(tx + 72) = now.tv_sec;
    *(int64_t *)(tx + 80) = now.tv_usec;
    *(int64_t *)(tx + 88) = 10000; // tick (us)
    return 0;                      // TIME_OK
}

// pidfd support: macOS has no pidfd, so pidfd_open() hands back a real (/dev/null) fd and we remember
// which guest pid it stands for, so pidfd_send_signal() can resolve the fd back to its target pid.
// Sized well above the guest's default RLIMIT_NOFILE so a pidfd-heavy runtime does not hit a capacity
// cliff (fail differently from Linux) once the table fills; entries are freed as their fds close.
#define PIDFD_MAX 4096

static struct {
    int fd;
    pid_t pid;
} g_pidfd[PIDFD_MAX];

// Returns 0 on success, -1 if the fixed table is full (caller then fails the open cleanly rather than
// handing back an fd that later can't be resolved by pidfd_lookup).
static int pidfd_register(int fd, pid_t pid) {
    for (int i = 0; i < PIDFD_MAX; i++)
        if (g_pidfd[i].fd == 0 || g_pidfd[i].fd == fd) {
            g_pidfd[i].fd = fd;
            g_pidfd[i].pid = pid;
            return 0;
        }
    return -1; // table exhausted
}

static int pidfd_lookup(int fd, pid_t *pid) {
    for (int i = 0; i < PIDFD_MAX; i++)
        if (g_pidfd[i].fd == fd) {
            *pid = g_pidfd[i].pid;
            return 0;
        }
    return -1;
}

// Drop a pidfd's table slot when the guest close()s it, so a spawn-heavy driver (go/npm/cargo forks
// thousands of children, one pidfd each) can't exhaust the fixed table.
static void pidfd_forget(int fd) {
    for (int i = 0; i < PIDFD_MAX; i++)
        if (g_pidfd[i].fd == fd) g_pidfd[i].fd = 0;
}

// Mint a pidfd for `pid`. macOS has no pidfd, so back it with a kqueue armed EVFILT_PROC/NOTE_EXIT on the
// process: the fd is pollable and goes readable exactly when `pid` exits (poll(2) directly, and epoll --
// itself a kqueue -- via EVFILT_READ on this nested kqueue). No EV_CLEAR, so the exit stays pending and the
// fd stays readable afterwards, matching Linux pidfd semantics. This is the load-bearing half of CLONE_PIDFD
// (go/rust/glibc-posix_spawn epoll_wait the returned pidfd to reap their compiler child). If the process is
// already gone or EVFILT_PROC can't arm (e.g. a non-child target), fall back to an always-readable /dev/null
// fd so a wait returns immediately instead of blocking forever. Registers the fd->pid map for
// waitid(P_PIDFD)/pidfd_send_signal. Returns -1 only if no fd could be opened at all.
static int pidfd_make(pid_t pid) {
    int kq = kqueue();
    if (kq >= 0) {
        struct kevent kv;
        EV_SET(&kv, (uintptr_t)pid, EVFILT_PROC, EV_ADD, NOTE_EXIT, 0, NULL);
        if (kevent(kq, &kv, 1, NULL, 0, NULL) == 0) {
            fcntl(kq, F_SETFD, FD_CLOEXEC);
            if (pidfd_register(kq, pid) != 0) { // table full: don't hand back an unresolvable fd
                close(kq);
                errno = EMFILE;
                return -1;
            }
            return kq;
        }
        close(kq);
    }
    int fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
    if (fd < 0) return -1;
    if (pidfd_register(fd, pid) != 0) { // table full: fail cleanly instead of leaking an unresolvable fd
        close(fd);
        errno = EMFILE;
        return -1;
    }
    return fd;
}

// POSIX message queues (mq_*): macOS has no POSIX mqueue, so emulate an in-process named priority queue.
// Each queue keeps messages highest-priority-first (FIFO within a priority); descriptors are real
// (/dev/null-backed) fds so close()/poll() stay valid, with an fd->queue table to map them back. This
// covers single-process producers/consumers. It is not shared across fork; a *blocking* mq_timed{send,
// receive} (O_NONBLOCK clear) honours the abs_timeout and polls the queue so another THREAD of the same
// process draining/filling it is observed, then returns ETIMEDOUT past the deadline (see rare.c). A NULL
// timeout blocks indefinitely, exactly as Linux would for the single-process case where nothing can change
// the queue -- genuinely unemulatable to "unblock" (documented at the call site), so it is left faithful.
#define MQ_MAXQ 16
#define MQ_MAXMSG 64
#define MQ_O_NONBLOCK 0x800 // Linux O_NONBLOCK (04000) on both x86_64 and aarch64; mq's per-descriptor flag

struct mq_qmsg {
    unsigned prio;
    size_t len;
    char *data;
};

struct mq_queue {
    int used, unlinked, refs, n;
    char name[260]; // POSIX mq name: leading '/' + component up to NAME_MAX(255) + NUL (ENAMETOOLONG beyond)
    long maxmsg, msgsize;
    struct mq_qmsg msg[MQ_MAXMSG];
    // mq_notify: the single registered one-shot notification, delivered on the empty->non-empty edge (see
    // mq_timedsend). Single-process-tree emulation, so notify_pid only backs the errno/one-shot semantics
    // (EBUSY when already owned), not real cross-process routing.
    int notify_set;      // 1 = a notification is currently registered
    int notify_notify;   // SIGEV_SIGNAL(0) / SIGEV_NONE(1) / SIGEV_THREAD(2)
    int notify_signo;    // signal to raise on the edge (SIGEV_SIGNAL)
    uint64_t notify_val; // sigev_value.sival_ptr/int -> the notification siginfo's si_value
    int notify_pid;      // registered owner (guest tgid)
};
static struct mq_queue g_mqq[MQ_MAXQ];

static struct {
    int fd, qi, flags; // flags: per-descriptor O_NONBLOCK (mq_flags is a per-open-file-description flag on Linux)
} g_mqfd[64];

static int mq_find(const char *name) {
    for (int i = 0; i < MQ_MAXQ; i++)
        if (g_mqq[i].used && !g_mqq[i].unlinked && !strcmp(g_mqq[i].name, name)) return i;
    return -1;
}

static int mq_qof(int fd) {
    for (int i = 0; i < 64; i++)
        if (g_mqfd[i].fd == fd) return g_mqfd[i].qi;
    return -1;
}

static void mq_bind(int fd, int qi) {
    for (int i = 0; i < 64; i++)
        if (g_mqfd[i].fd == 0 || g_mqfd[i].fd == fd) {
            g_mqfd[i].fd = fd;
            g_mqfd[i].qi = qi;
            return;
        }
}

// Per-descriptor O_NONBLOCK: report/set the mq_flags of the open file description behind fd. Recorded at
// mq_open time and toggled by mq_getsetattr (the mq equivalent of F_SETFL) so a blocking descriptor and a
// non-blocking one to the same queue behave differently, as on Linux.
static int mq_fd_nonblock(int fd) {
    for (int i = 0; i < 64; i++)
        if (g_mqfd[i].fd == fd) return (g_mqfd[i].flags & MQ_O_NONBLOCK) != 0;
    return 0;
}

static void mq_fd_setnb(int fd, int on) {
    for (int i = 0; i < 64; i++)
        if (g_mqfd[i].fd == fd) {
            g_mqfd[i].flags = on ? MQ_O_NONBLOCK : 0;
            return;
        }
}

static void mq_maybe_free(int qi) {
    struct mq_queue *q = &g_mqq[qi];
    if (q->refs <= 0 && q->unlinked) {
        for (int j = 0; j < q->n; j++)
            free(q->msg[j].data);
        memset(q, 0, sizeof *q);
    }
}

// CPU topology: the number of CPUs to advertise to the guest (the host's online count, capped). glibc
// and tcmalloc enumerate CPUs via sched_getaffinity and /sys/devices/system/cpu/{online,possible};
// reporting only CPU 0 makes tcmalloc's NumPossibleCPUs() assert (`cpus.has_value()`) and mongod abort.
static int dd_online_cpus(void) {
    // container_online_cpus() (state.c) applies the docker --cpus cap (ceil(NanoCpus/1e9)) on top of the
    // host online count, so sched_getaffinity / the cpu-topology sysfs advertise the container's allotment.
    return container_online_cpus();
}

// Build the "all online CPUs" bitmask into the caller's buffer (CPU i -> bit i, little-endian bytes).
static void cpu_online_mask(uint8_t *m, size_t n) {
    memset(m, 0, n);
    int nc = dd_online_cpus();
    for (int cpu = 0; cpu < nc; cpu++)
        if ((size_t)(cpu / 8) < n) m[cpu / 8] |= (uint8_t)(1u << (cpu % 8));
}

// Current CPU-affinity mask (process-global; default = all online CPUs). sched_setaffinity records the
// guest's chosen mask so sched_getaffinity round-trips it (pin-to-CPU0 then read back), while a fresh
// process still advertises every online CPU so glibc/tcmalloc size their per-CPU tables correctly.
static uint8_t g_affinity[128];
static int g_affinity_set;

static const uint8_t *affinity_mask(void) {
    if (!g_affinity_set) {
        cpu_online_mask(g_affinity, sizeof g_affinity);
        g_affinity_set = 1;
    }
    return g_affinity;
}

// Lowest CPU id in the current affinity mask. getcpu(2) must return a CPU the task is allowed to run
// on; when the guest has pinned itself to a single CPU via sched_setaffinity, that is the exact value
// LTP getcpu01 expects back. Falls back to CPU 0 for an (impossible) empty mask.
static unsigned affinity_first_cpu(void) {
    const uint8_t *m = affinity_mask();
    for (size_t i = 0; i < sizeof g_affinity; i++)
        if (m[i])
            for (int b = 0; b < 8; b++)
                if (m[i] & (1u << b)) return (unsigned)(i * 8 + b);
    return 0;
}

// Back a short synthesized sysfs string with an anonymous temp fd (the same trick proc_open uses for
// the macOS-has-no-/proc case). Returns a readable fd positioned at offset 0, or -1 on error.
static int synth_str_fd(const char *s) {
    char tn[] = "/tmp/.ddcpuXXXXXX";
    int fd = mkstemp(tn);
    if (fd < 0) return -1;
    unlink(tn);
    size_t len = strlen(s);
    if (write(fd, s, len) < 0) {}
    lseek(fd, 0, SEEK_SET);
    return fd;
}

// Render the kernel's CPU-range format ("0" for a single CPU, else "0-N\n") for the cpu/{online,
// possible,present} sysfs files that glibc __get_nprocs / tcmalloc NumPossibleCPUs parse.
static void cpu_range_str(char *buf, size_t n) {
    int nc = dd_online_cpus();
    if (nc <= 1)
        snprintf(buf, n, "0\n");
    else
        snprintf(buf, n, "0-%d\n", nc - 1);
}

// /proc/self/exe and /proc/<pid>/exe (where <pid> is the guest's own pid) are magic kernel symlinks
// to the running executable. macOS has no /proc, so synthesize them: the link target is the guest
// path that was exec'd (g_exe_path). Many programs (Go, the JVM, boost::filesystem, mongod) readlink
// or stat this to locate their own binary. Returns 1 and fills tgt[] with the guest-visible target
// path when `p` names this link, else 0.
// Backing storage for g_exe_path after an execve (case 221) updates it: the initial g_exe_path points at
// the launcher's argv buffer, but a post-exec /proc/self/exe must name the NEWLY exec'd image. We copy the
// guest-absolute exec path here and repoint g_exe_path at it (the prior image's address space is torn down).
static char g_exe_path_store[4200];

static int proc_self_exe(const char *p, char *tgt, size_t cap) {
    if (!p || strncmp(p, "/proc/", 6)) return 0;
    const char *rest = p + 6;
    if (!strncmp(rest, "self/", 5)) {
        rest += 5;
    } else if (!strncmp(rest, "thread-self/", 12)) {
        rest += 12; // /proc/thread-self/exe: same file (single thread group leader identity)
    } else {
        char *end;
        long pid = strtol(rest, &end, 10);
        if (end == rest || *end != '/') return 0;
        if ((int)pid != container_pid() && (int)pid != (int)getpid()) {
            // a PEER container process's /proc/<pid>/exe: serve its published canonical exe path
            // (each engine process publishes it at boot + execve -- see proc_reg_publish).
            int host;
            if (strcmp(end + 1, "exe") || !proc_pid_member((int)pid, &host)) return 0;
            return proc_reg_exe_read(host, tgt, cap);
        }
        rest = end + 1;
    }
    if (strcmp(rest, "exe")) return 0;
    const char *src = (g_exe_path && g_exe_path[0]) ? g_exe_path : "/";
    // g_exe_path is already a guest-absolute path; strip any rootfs prefix that may have leaked in.
    if (g_rootfs && !strncmp(src, g_rootfs_canon, g_rootfs_canon_len)) src += g_rootfs_canon_len;
    if (!src[0]) src = "/";
    size_t l = strlen(src);
    if (l >= cap) l = cap - 1;
    memcpy(tgt, src, l);
    tgt[l] = 0;
    return 1;
}

// Guest-ABSOLUTE, lexically-normalized form of an *at() path -- so the /proc magic-link synthesis
// matches however the caller names the link: absolute, relative to the guest cwd, or relative to a
// dir-fd (readlinkat(pid_dirfd, "exe") -- readlink-vs-readlinkat consistency). Symlinks are
// NOT resolved; a joined path that matches no /proc form simply falls through to real resolution.
static void guest_abspath_at(int dirfd, const char *raw, char *out, size_t n) {
    char j[8600];
    if (!raw) {
        snprintf(out, n, "/");
        return;
    }
    if (raw[0] == '/')
        snprintf(j, sizeof j, "%s", raw);
    else if (dirfd >= 0) {
        if (dirfd < 1024 && g_fdpath[dirfd][0]) {
            if (g_rootfs)
                abs_guest(dirfd, raw, j, sizeof j);
            else
                snprintf(j, sizeof j, "%s/%s", g_fdpath[dirfd], raw); // bare: guest view == host view
        } else {
            char db[4200];
            if (fcntl(dirfd, F_GETPATH, db) == 0) {
                if (g_rootfs) {
                    char gd[4200];
                    guest_from_host_raw(db, gd, sizeof gd);
                    snprintf(j, sizeof j, "%s/%s", gd, raw);
                } else
                    snprintf(j, sizeof j, "%s/%s", db, raw);
            } else
                snprintf(j, sizeof j, "%s/%s", g_cwd, raw);
        }
    } else { // AT_FDCWD
        if (g_rootfs)
            snprintf(j, sizeof j, "%s/%s", g_cwd[0] ? g_cwd : "/", raw);
        else {
            char cw[4200];
            if (!getcwd(cw, sizeof cw)) cw[0] = 0; // bare: the engine chdir()s for real
            snprintf(j, sizeof j, "%s/%s", cw, raw);
        }
    }
    path_norm_lex(j, out, n);
}

// svc_fs/svc_proc/svc_rare live here (not with the other family includes at the top): their cases call
// this file's local helpers (overlay_*/proc_self_exe/synth_str_fd for fs; nonpie_p/cpu_online_mask/
// affinity_mask for proc; svc_adjtimex/pidfd_*/mq_* for rare) defined just above, so they must be
// included AFTER them.
#include "fs.c"
#include "proc.c"
#include "rare.c"
#include "ptrace.c" // bug real ptrace tracer/tracee coordination (uses helpers above + G_* macros)

static void service(struct cpu *c) {
    // Mark this thread as "in a host syscall" for the whole service window (incl. any blocking wait such
    // as pause()/ppoll()/read()): the fault-class-signal handlers use it to tell an external kill of
    // SIGSEGV/ILL/FPE/... from a genuine engine fault (see g_in_service in os/linux/signal.c). Cleared on
    // EVERY exit path below, so the ptrace/untrusted routes must not early-return past the clear.
    g_in_service = 1;
    uint64_t _rnr = g_systrace ? G_NR(c) : 0; // JTS: capture nr to pair the return log below
    // seccomp gate: run the guest's installed cBPF filter(s) / STRICT policy against this syscall BEFORE it
    // is routed anywhere. On an intercepted syscall (ERRNO/TRAP/TRACE/KILL/strict-violation) the result is
    // already set in G_RET / a signal is queued / the process is killed, so we must NOT service it. Inert
    // (one predicted-not-taken load) until a guest installs a filter. Runs on the RAW guest register state,
    // before x86 legacy-syscall normalization, so the filter sees the number/args the guest actually issued.
    if (__builtin_expect(seccomp_gate(c) != 0, 0)) {
        if (g_systrace)
            fprintf(stderr, "[ret pid=%d] %llu -> %lld (seccomp)\n", (int)getpid(), (unsigned long long)_rnr,
                    (long long)(int64_t)G_RET(c));
        g_in_service = 0;
        return;
    }
    if (__builtin_expect(g_untrusted, 0)) {
        syscall_route(c); // untrusted: route via sentry
    } else if (__builtin_expect(g_pt != NULL && __atomic_load_n(&g_pt->nactive, __ATOMIC_RELAXED) != 0, 0)) {
        // any guest process under ptrace -> route through the traced dispatcher so this syscall can
        // syscall-stop (entry/exit) for its tracer. One relaxed shared load; not taken for the whole matrix.
        ptrace_service_traced(c);
    } else {
        service_local(c); // trusted: byte-identical path
    }
    if (g_systrace)
        fprintf(stderr, "[ret pid=%d] %llu -> %lld\n", (int)getpid(), (unsigned long long)_rnr,
                (long long)(int64_t)G_RET(c));
    g_in_service = 0;
}

static void service_local(struct cpu *c) {
    // Frontends whose guest has legacy syscalls without a canonical (aarch64) equivalent rewrite them
    // into their *at form here (x86: open->openat, ...); a no-op where the guest is already canonical.
    if (G_NORMALIZE(c)) return;
    // we reached service by executing a guest syscall instruction, so this task was RUNNING; publish
    // 'R' (and claim/refresh our cross-process task-state slot -- also re-claims after fork on getpid change).
    // A handler that then parks in a blocking host wait stamps 'S' for its duration and 'R' again on wake.
    ts_running();
    uint64_t nr = G_NR(c), a0 = G_A0(c), a1 = G_A1(c), a2 = G_A2(c), a3 = G_A3(c), a4 = G_A4(c), a5 = G_A5(c);
    if (g_trace || g_systrace)
        fprintf(stderr, "[sys pid=%d] %llu (%llx,%llx,%llx,%llx,%llx,%llx)\n", (int)getpid(), (unsigned long long)nr,
                (unsigned long long)a0, (unsigned long long)a1, (unsigned long long)a2, (unsigned long long)a3,
                (unsigned long long)a4, (unsigned long long)a5);
    // --- non-PIE ET_EXEC pointer-arg redirect (g2h) --------------------------------------------------
    // Rebase ONLY the pointer-typed args of each syscall a non-PIE realistically hands a low-image
    // (.rodata/.data/.bss) pointer to, so the host syscall reads/writes the SAME bytes a native run would.
    // Per-syscall + per-position: size/flag/fd/count args are NEVER touched (a blanket a0..a5 rebase would
    // corrupt a count/fd that happened to fall in the link range). Whole block is inert unless g_nonpie_lo
    // is set (ET_EXEC only) -> PIE/static-PIE and the entire test matrix are byte-identical. Numbers are
    // the canonical (aarch64) syscall numbers G_NR() maps the x86 guest's calls onto. Runs BEFORE the
    // res_bump switch below, which itself dereferences a2 (open_how*) for openat2.
    if (g_nonpie_lo) {
        switch (nr) {
        case 56:  // openat(dfd, PATH, flags, mode)
        case 33:  // mknodat(dfd, PATH, ...)
        case 34:  // mkdirat(dfd, PATH, mode)
        case 35:  // unlinkat(dfd, PATH, flags)
        case 48:  // faccessat(dfd, PATH, mode)
        case 439: // faccessat2(dfd, PATH, mode, flags)
        case 53:  // fchmodat(dfd, PATH, mode, flags)
        case 54:  // fchownat(dfd, PATH, uid, gid, flags)
            a1 = nonpie_p(a1);
            break; //   path is a1 for the whole *at family
        case 437:
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break; // openat2(dfd, PATH, open_how*, size)
        case 79:   // newfstatat(dfd, PATH, STATBUF, flags)
        case 78:
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break; // readlinkat(dfd, PATH, BUF, sz)
        case 88:   // utimensat(dfd, PATH, TIMES[2], flags) -- sibling of fchmodat/fchownat in the *at
                   // metadata family. Both the path (a1) and the struct timespec[2] TIMES (a2) can be
                   // low link-vaddr pointers in a non-PIE (glibc's utime/utimes/utimensat + LTP's
                   // SAFE_TOUCH pass .rodata/.bss addresses); without the rebase the host utimensat reads
                   // an unmapped low address and EFAULTs (LTP link02/link05/lstat01/lstat02 BROK in
                   // SAFE_TOUCH setup). a1==NULL (futimens-by-fd) / a2==NULL (times=now) stay 0 via nonpie_p.
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break;
        case 291:
            a1 = nonpie_p(a1);
            a4 = nonpie_p(a4);
            break; // statx(dfd, PATH, flags, mask, STATXBUF)
        case 36:
            a0 = nonpie_p(a0);
            a2 = nonpie_p(a2);
            break; // symlinkat(TARGET, newdfd, LINKPATH)
        case 37:   // linkat(odfd, OLD, ndfd, NEW, flags)
        case 38:   // renameat(odfd, OLD, ndfd, NEW)
        case 276:
            a1 = nonpie_p(a1);
            a3 = nonpie_p(a3);
            break;                          // renameat2(odfd, OLD, ndfd, NEW, flags)
        case 80:                            // fstat(fd, STATBUF)
        case 63:                            // read(fd, BUF, count)
        case 64:                            // write(fd, BUF, count)
        case 67:                            // pread64(fd, BUF, count, off)
        case 68:                            // pwrite64(fd, BUF, count, off)
        case 200:                           // bind(fd, SOCKADDR, alen)
        case 203:                           // connect(fd, SOCKADDR, alen)
        case 204:                           // getsockname(fd, ADDR, alen)
        case 205:                           // getpeername(fd, ADDR, alen)
        case 202:                           // accept(fd, ADDR, alen)
        case 242:                           // accept4(fd, ADDR, alen, flags)
        case 61:                            // getdents64(fd, DIRENT_BUF, count)
        case 113: a1 = nonpie_p(a1); break; // clock_gettime(clkid, TIMESPEC)
        case 25:                            // fcntl(fd, cmd, ARG): ARG is a struct flock* ONLY for the record-lock
            if (a1 == 5 || a1 == 6 || a1 == 7) a2 = nonpie_p(a2); // cmds F_GETLK/F_SETLK/F_SETLKW (else it is an
            break; //   int flag/floor arg, never a pointer, so leave it untouched). The
                   //   handler dereferences the flock directly (host_range_mapped + reads),
                   //   so a low link-vaddr flock in a non-PIE (LTP fcntl05/fcntl13) must be
                   //   rebased or the guard EFAULTs on the unmapped low address.
        // iovec-carrying calls -- rebase the array base AND every entry's iov_base. A non-PIE's
        // gather/scatter buffers can themselves be low link-vaddr pointers (skalibs' buffer_1 flush issues
        // writev(fd, iov, n) whose iov_base entries point at .rodata baked at 0x40xxxx). Rebasing only the
        // array base (the old behaviour) left the inner pointers LOW, where nothing is mapped -> the host
        // writev EFAULTs and writes nothing. That is exactly why s6-overlay-stat printed an EMPTY line:
        // s6-overlay preinit's `eval $(s6-overlay-stat /run)` then left $uid unset, so `test "$UID" -ne ""`
        // hit busybox's empty-operand path -> "sh: out of range" and the s6-overlay-v3 boot aborted (111).
        // The rebased copy lives in a per-thread scratch array consumed synchronously by svc_io below.
        case 65:   // readv(fd, IOVEC, n)
        case 66:   // writev(fd, IOVEC, n)
        case 69:   // preadv(fd, IOVEC, n, off)
        case 286:  // preadv2(fd, IOVEC, n, off, off_hi, flags) -- same (iov=a1, iovcnt=a2) shape
        case 287:  // pwritev2(fd, IOVEC, n, off, off_hi, flags) -- same shape; inner iov_base rebased too
        case 70: { // pwritev(fd, IOVEC, n, off)
            a1 = nonpie_p(a1);
            int niov = (int)a2;
            if (niov > 0 && niov <= 1024 && host_range_mapped((uintptr_t)a1, (size_t)niov * sizeof(struct iovec))) {
                static _Thread_local struct iovec reb[1024];
                const struct iovec *src = (const struct iovec *)a1;
                for (int i = 0; i < niov; i++) {
                    reb[i].iov_base = (void *)nonpie_p((uint64_t)(uintptr_t)src[i].iov_base);
                    reb[i].iov_len = src[i].iov_len;
                }
                a1 = (uint64_t)(uintptr_t)reb;
            }
            break;
        }
        case 17:                            // getcwd(BUF, size)
        case 160: a0 = nonpie_p(a0); break; // uname(UTSBUF)
        case 73:                            // ppoll(FDS, n, TMO, sigmask, sz): the handler dereferences BOTH
            a0 = nonpie_p(a0);              //   the pollfd array (a0) AND the timespec deadline (a2, read for
            a2 = nonpie_p(a2);              //   the budget and written back with the remaining time). sigmask
            break;                          //   (a3) is ignored by the handler, so only a0+a2 need rebasing.
        case 207:                           // recvfrom(fd, BUF, len, fl, SRCADDR, alen)
        case 206:
            a1 = nonpie_p(a1);
            a4 = nonpie_p(a4);
            break;                          // sendto(fd, BUF, len, fl, SOCKADDR, alen)
        case 211:                           // sendmsg(fd, MSGHDR, flags) -- top only
        case 212: a1 = nonpie_p(a1); break; // recvmsg(fd, MSGHDR, flags) -- top only
        case 221:
            a0 = nonpie_p(a0);
            a1 = nonpie_p(a1);
            break; // execve(PATH, ARGV, envp); argv base here,
                   //   each argv[] element rebased at case 221
        case 281:  // execveat(dfd, PATH, ARGV, envp, flags) -- mirrors 221 (path + argv base; elements
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break; //   rebased at the shared case-221 body after the case-281 arg shift)
        // Syscalls whose result the ENGINE writes/reads into the guest buffer ITSELF (memset/memcpy/
        // struct fill / arc4random_buf), not via a host syscall -- so there is no host EFAULT fixup to
        // rescue a low, un-rebased non-PIE pointer; the handler's host_range_mapped() guard would simply
        // fail on the unmapped low address. Rebase the buffer arg BEFORE the handler runs. ((a):
        // getrandom's a0 was the one that made python3.11-x86 EFAULT in _Py_HashRandomization_Init.)
        case 169: // gettimeofday(TIMEVAL, TZ) -- the engine writes BOTH tv (a0) and the deprecated tz (a1)
            a0 = nonpie_p(a0);
            a1 = nonpie_p(a1);
            break;
        case 278: // getrandom(BUF, len, flags)      -- buffer is a0
        case 179: // sysinfo(INFOBUF)
        case 153: // times(TMSBUF)
        case 236: // get_mempolicy(MODE, ...)        -- mode ptr is a0
        case 161: // sethostname(NAME, len)          -- name buffer is a0
        case 59:  // pipe2(FDS, flags) -- the two result fds are written into a0 by the engine itself, so a
                  //   low non-PIE fds[] (skalibs/s6-linux-init pass a .bss array at 0x42xxxx) must be rebased
                  //   or the handler's host_range_mapped guard EFAULTs ("unable to pipe: Bad address")
            a0 = nonpie_p(a0);
            break;
        case 165: // getrusage(who, RUSAGEBUF)       -- buffer is a1
        case 114: // clock_getres(clkid, TIMESPEC)
        case 127: // sched_rr_get_interval(pid, TIMESPEC)
        case 44:  // fstatfs(fd, STATFSBUF)
            a1 = nonpie_p(a1);
            break;
        case 122: // sched_setaffinity(pid, len, MASK)  -- mask read directly (a1 is a size, never rebased)
        case 123: // sched_getaffinity(pid, len, MASK)  -- mask written directly
        case 115: // clock_nanosleep(clkid, flags, REQUEST, remain) -- req read directly in the ABSTIME loop
            a2 = nonpie_p(a2);
            break;
        case 232: // mincore(ADDR, len, VEC) -- vec is written directly by the engine; addr may name image
                  //   pages (mincore of the binary's own mapping) so rebase both. An mmap result is high and
                  //   outside [nonpie_lo,hi) so nonpie_p leaves it. (LTP mincore02's vec is a .bss static.)
            a0 = nonpie_p(a0);
            a2 = nonpie_p(a2);
            break;
        case 101: // nanosleep(REQUEST, remain) -- both read/written directly by the engine's deadline loop
            a0 = nonpie_p(a0);
            a1 = nonpie_p(a1);
            break;
        case 261: // prlimit64(pid, res, NEW, OLD) -- NEW read (a2) + OLD written (a3), both derefed by the
                  // handler (proc.c case 261) with NO host_range_mapped guard. glibc's setrlimit() funnels to
                  // prlimit64(pid,res,&new,NULL), so a non-PIE static binary's `static struct rlimit` NEW is a
                  // low .bss link vaddr -- rebase a2 as well or the unguarded `nl[0]/nl[1]` read SIGSEGVs on
                  // the unmapped low address. (Latent until x86 lea stopped pre-biasing pointers HIGH: on
                  // aarch64 the low a2 fault was silently served by nonpie_fixup; x86 hard-crashed. Rebasing
                  // here fixes both arches directly, no fault-path reliance.)
            a2 = nonpie_p(a2);
            a3 = nonpie_p(a3);
            break;
        case 43:  // statfs(PATH, STATFSBUF)         -- path read + buffer written
        case 168: // getcpu(CPU, NODE, tcache)       -- cpu + node written
            a0 = nonpie_p(a0);
            a1 = nonpie_p(a1);
            break;
        // the remaining PATH-taking fs syscalls a non-PIE hands a low.rodata/.bss pointer to. Without
        // the rebase the host syscall (or the engine's own resolve/copy) dereferences the un-relocated low
        // link vaddr -> EFAULT/SIGSEGV on a VALID guest pointer (arm64 LTP truncate02/getcwd02 static-EXEC).
        // These mirror the *at family above but are the "bare path" (a0) or fd+name/value forms.
        case 45: // truncate(PATH, length)            -- path a0 (length is a scalar, never rebased)
        case 49: // chdir(PATH)                        -- path a0
        case 51: // chroot(PATH)                       -- path a0
            a0 = nonpie_p(a0);
            break;
        case 5: // setxattr(PATH, NAME, VALUE, size, flags)
        case 6: // lsetxattr(PATH, NAME, VALUE, size, flags)
        case 8: // getxattr(PATH, NAME, VALUE, size)
        case 9: // lgetxattr(PATH, NAME, VALUE, size)
            a0 = nonpie_p(a0);
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break;
        case 7:  // fsetxattr(fd, NAME, VALUE, size, flags)   -- a0 is an fd
        case 10: // fgetxattr(fd, NAME, VALUE, size)          -- a0 is an fd
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break;
        case 11: // listxattr(PATH, LIST, size)
        case 12: // llistxattr(PATH, LIST, size)
        case 14: // removexattr(PATH, NAME)
        case 15: // lremovexattr(PATH, NAME)
            a0 = nonpie_p(a0);
            a1 = nonpie_p(a1);
            break;
        case 13: // flistxattr(fd, LIST, size)                -- a0 is an fd
        case 16: // fremovexattr(fd, NAME)                    -- a0 is an fd
            a1 = nonpie_p(a1);
            break;
        case 264: // name_to_handle_at(dfd, PATH, HANDLE, MOUNT_ID, flags)
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            a3 = nonpie_p(a3);
            break;
        // Struct-writer/reader time syscalls the engine fills/reads via the guest pointer directly (same
        // class as sysinfo/times/gettimeofday/getrusage above) -- rebase the low non-PIE struct pointer.
        case 102: // getitimer(which, CURR_VALUE)       -- itimerval written to a1
        case 266: // clock_adjtime(clkid, TIMEX)        -- timex read+written at a1
            a1 = nonpie_p(a1);
            break;
        case 103: // setitimer(which, NEW_VALUE, OLD_VALUE)
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break;
        case 171: // adjtimex(TIMEX)                     -- timex read+written at a0
            a0 = nonpie_p(a0);
            break;
        // timer / timerfd / sched / signalfd / epoll_ctl handlers dereference their struct pointers
        // directly (itimerspec / sigevent / sched_param / sigset / epoll_event), so a low link-vaddr pointer
        // in a non-PIE (LTP's static test binaries put these in .bss/.data at ~0x52xxxx) must be rebased or
        // the handler's guest_bad_ptr guard EFAULTs on the unmapped low address.
        case 74:  // signalfd4(fd, MASK, sizemask, flags)     -- sigset read directly
        case 87:  // timerfd_gettime(fd, CURR)                -- itimerspec written by the engine
        case 108: // timer_gettime(timerid, CURR)
        case 118: // sched_setparam(pid, PARAM)               -- sched_param read directly
        case 121: // sched_getparam(pid, PARAM)               -- sched_param written by the engine
            a1 = nonpie_p(a1);
            break;
        case 119: // sched_setscheduler(pid, policy, PARAM)   -- sched_param read directly
            a2 = nonpie_p(a2);
            break;
        case 21: // epoll_ctl(epfd, op, fd, EVENT)           -- epoll_event read directly
            a3 = nonpie_p(a3);
            break;
        case 86:  // timerfd_settime(fd, flags, NEW, OLD)     -- new read / old written
        case 110: // timer_settime(timerid, flags, NEW, OLD)
            a2 = nonpie_p(a2);
            a3 = nonpie_p(a3);
            break;
        case 107: // timer_create(clockid, SIGEVENT, TIMERID) -- sigevent read / timer id written
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break;
        // the remaining pointer-arg syscalls whose handler dereferences the guest pointer DIRECTLY (a
        // host-syscall deref, or the engine reading/writing the guest struct itself) that were still missing
        // from this switch -- so a non-PIE guest handing a low .bss/.rodata/.data pointer EFAULTed (or, for the
        // unguarded handlers, SIGSEGV'd the engine) on a VALID pointer. This is the getgroups/semop/msgsnd
        // report plus the WHOLE class audited alongside it: the credential, SysV-IPC, rt_signal, sched/rlimit,
        // poll/select and POSIX-mqueue families. Numbers are the aarch64-canonical ones the x86 guest is
        // normalized onto BEFORE this switch; on x86 the arg was already biased HIGH by the loader/translator
        // (elf.c/mov.c), so nonpie_p is inert there -> ONE case covers both arches with no double-rebase,
        // exactly like the shared cases above. (Inert for PIE/static-PIE: the whole switch is gated on
        // g_nonpie_lo, which the entire test matrix leaves 0.)
        // -- credentials (proc.c: the buffers are written directly / guarded by guest_bad_ptr) --
        case 90: // capget(HDRP, DATAP)
        case 91: // capset(HDRP, DATAP)
            a0 = nonpie_p(a0);
            a1 = nonpie_p(a1);
            break;
        case 148: // getresuid(RUID, EUID, SUID) -- all three written directly
        case 150: // getresgid(RGID, EGID, SGID)
            a0 = nonpie_p(a0);
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break;
        case 158: // getgroups(size, LIST) -- list written directly (or via host getgroups)
            a1 = nonpie_p(a1);
            break;
        // -- SysV IPC (sysv.c) --
        case 188: // msgrcv(msqid, MSGP, sz, typ, flg) -- msgp written by host msgrcv
        case 189: // msgsnd(msqid, MSGP, sz, flg)      -- msgp read by host msgsnd
        case 193: // semop(semid, SOPS, nsops)         -- sops read by host semop
            a1 = nonpie_p(a1);
            break;
        case 192:              // semtimedop(semid, SOPS, nsops, TIMEOUT) -- sops (+timeout; harmless if the handler,
            a1 = nonpie_p(a1); //   which routes to semop, ignores it)
            a3 = nonpie_p(a3);
            break;
        case 191: // semctl(semid, semnum, CMD, arg): arg(a3) is a pointer ONLY for GETALL(13)/SETALL(17);
            if (a2 == 13 || a2 == 17) a3 = nonpie_p(a3); //   SETVAL(16)'s a3 is an int val -> never rebased
            break;
        case 195: // shmctl(shmid, cmd, BUF): IPC_STAT marshals the host struct into buf(a2) directly
            a2 = nonpie_p(a2);
            break;
        // -- rt_signal family (signal.c, + rt_tgsigqueueinfo in rare.c): the sigset/siginfo/sigaction/altstack/
        //    timespec structs are read or written through the guest pointer directly (rt_sigaction EFAULTs via
        //    host_range_mapped on a low ptr; the others would fault the engine) --
        case 132: // sigaltstack(NEW, OLD)         -- new read, old written
        case 134: // rt_sigaction(sig, ACT, OLD)   -- act read, old written
        case 135: // rt_sigprocmask(how, SET, OLD) -- set read, old written
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break;
        case 133: // rt_sigsuspend(UNEWSET, sz) -- mask read directly
        case 136: // rt_sigpending(SET, sz)     -- pending set written directly
            a0 = nonpie_p(a0);
            break;
        case 137: // rt_sigtimedwait(SET, INFO, TIMEOUT, sz) -- set read, info written, timeout read
            a0 = nonpie_p(a0);
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break;
        case 138: // rt_sigqueueinfo(tgid, sig, INFO)        -- siginfo read directly
            a2 = nonpie_p(a2);
            break;
        case 240: // rt_tgsigqueueinfo(tgid, tid, sig, INFO)  -- siginfo read directly (rare.c handler)
            a3 = nonpie_p(a3);
            break;
        // -- sched / rlimit / wait (rare.c + proc.c) --
        case 95: // waitid(idtype, id, INFOP, options) -- siginfo written (host_range_mapped guard EFAULTs low)
            a2 = nonpie_p(a2);
            break;
        case 98:               // futex(UADDR, op, val, TIMEOUT/nr_wake2, UADDR2, val3) -- uaddr/timeout/uaddr2 are
            a0 = nonpie_p(a0); //   dereferenced by futex_op; a non-PIE static libc's lock word / timespec live in
            a3 = nonpie_p(a3); //   .bss at a low link vaddr. uaddr2 (a4) is the REQUEUE/WAKE_OP target -- a real
            a4 = nonpie_p(a4); //   guest pointer, so rebase it too (inert for PIE; a3-as-nr_wake2 is a small int
            break;             //   below g_nonpie_lo, so nonpie_p leaves it unchanged).
        case 163:              // getrlimit(res, RLIM) -- rlim written
        case 164:              // setrlimit(res, RLIM) -- rlim read
        case 275:              // sched_getattr(pid, ATTR, size, flags) -- attr zeroed+written directly
            a1 = nonpie_p(a1);
            break;
        case 260: // wait4(pid, STATUS, opts, RUSAGE) -- status + rusage written directly
            a1 = nonpie_p(a1);
            a3 = nonpie_p(a3);
            break;
        // -- poll / select (event.c): the pollfd/fd_set/timespec buffers are read+written directly --
        case 22: // epoll_pwait(epfd, EVENTS, max, tmo, SIGMASK) -- events written (sigmask a4 handler-ignored)
            a1 = nonpie_p(a1);
            a4 = nonpie_p(a4);
            break;
        case 72: // pselect6(n, READFDS, WRITEFDS, EXCEPTFDS, TIMEOUT, sigmask) -- all four deref'd directly
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            a3 = nonpie_p(a3);
            a4 = nonpie_p(a4);
            break;
        // -- POSIX message queues (rare.c: name/msg/attr/timeout read or written directly) --
        case 180: // mq_open(NAME, oflag, mode, ATTR) -- name string + attr read
            a0 = nonpie_p(a0);
            a3 = nonpie_p(a3);
            break;
        case 181: // mq_unlink(NAME)
            a0 = nonpie_p(a0);
            break;
        case 182: // mq_timedsend(mqdes, MSG, len, prio, TIMEOUT) -- msg read (+timeout)
            a1 = nonpie_p(a1);
            a4 = nonpie_p(a4);
            break;
        case 183: // mq_timedreceive(mqdes, MSG, len, PRIO, TIMEOUT) -- msg + prio written (+timeout)
            a1 = nonpie_p(a1);
            a3 = nonpie_p(a3);
            a4 = nonpie_p(a4);
            break;
        case 185: // mq_getsetattr(mqdes, NEWATTR, OLDATTR) -- oldattr written (newattr ignored by handler)
            a1 = nonpie_p(a1);
            a2 = nonpie_p(a2);
            break;
        default: break;
        }
    }
    // daemon-write coherence: notice a daemon-side write into this container's fs (docker cp /
    // exec-spawn /etc rewrites) and drop the path/metadata caches BEFORE any handler below can consult
    // them -- one shared-page atomic load per syscall (see fscache.c fsgen_poll).
    fsgen_poll();
    // S2 path-resolution-cache invalidation: bump the epoch BEFORE dispatch on any syscall that mutates
    // the FS namespace, so no cached guest->host string mapping can survive a create/unlink/rename/mkdir/
    // symlink (over-invalidates, never under -- when in doubt, the next lookup MISSES and re-resolves).
    // Legacy x86 forms (open/mkdir/rename/...) were already normalized to these *at numbers by G_NORMALIZE.
    switch (nr) {
    case 33:  // mknodat
    case 34:  // mkdirat
    case 35:  // unlinkat (covers rmdir via AT_REMOVEDIR)
    case 36:  // symlinkat
    case 37:  // linkat
    case 38:  // renameat
    case 39:  // umount2
    case 40:  // mount
    case 276: // renameat2
        res_bump();
        break;
    case 56: // openat: a2 = Linux flags. O_CREAT (0x40) adds a name. In OVERLAY mode a write-open
             // (O_WRONLY/O_RDWR, a2&3) copies the file lower->upper, RELOCATING its resolved host path
             // -- so it must invalidate too, or a cached lower path goes stale (flat rootfs: no copy-up).
        if ((a2 & 0x40) || (g_nlower && (a2 & 3))) res_bump();
        break;
    case 437: { // openat2: flags live in open_how.flags (a2 -> struct open_how *), before its case rewrites a2
        // a2 is a raw guest pointer to struct open_how; peek how[0] (flags) only if it is actually mapped, so
        // a bad pointer doesn't fault the engine in this pre-dispatch cache-invalidation probe. If it is
        // unmapped we simply skip the res_bump -- the real openat2 handler (svc_fs) returns -EFAULT below.
        uint64_t *how = (uint64_t *)a2;
        if (how && host_range_mapped((uintptr_t)a2, sizeof(uint64_t)) &&
            ((how[0] & 0x40) || (g_nlower && (how[0] & 3))))
            res_bump();
        break;
    }
    default: break;
    }
    if (svc_sysv(c, nr, a0, a1, a2, a3, a4, a5)) return;
    if (svc_mem(c, nr, a0, a1, a2, a3, a4, a5)) return;
    if (svc_signal(c, nr, a0, a1, a2, a3, a4, a5)) return;
    if (svc_time(c, nr, a0, a1, a2, a3, a4, a5)) return;
    if (svc_io(c, nr, a0, a1, a2, a3, a4, a5)) return;
    if (svc_aio(c, nr, a0, a1, a2, a3, a4, a5)) return; // kernel-AIO/libaio (canonical 0-4): nginx/innodb file-AIO
    if (svc_fs(c, nr, a0, a1, a2, a3, a4, a5)) return;
    if (svc_proc(c, nr, a0, a1, a2, a3, a4, a5)) return;
    if (svc_net(c, nr, a0, a1, a2, a3, a4, a5)) return;
    if (svc_event(c, nr, a0, a1, a2, a3, a4, a5)) return;
    if (svc_misc(c, nr, a0, a1, a2, a3, a4, a5)) return;
    if (svc_rare(c, nr, a0, a1, a2, a3, a4, a5)) return;
    // ===================== unhandled =====================
    // Every Linux syscall is now owned by one of the svc_*() family modules above; reaching here means no
    // family claimed this number -> ENOSYS (the guest sees ENOSYS and falls back, exactly as on a real kernel
    // that lacks the syscall). this report used to be UNCONDITIONAL and went to fd 2 -- i.e. the
    // GUEST's stderr, not the engine's. A guest that pokes an unimplemented number in a hot loop (the arm64 Go
    // toolchain does, per goroutine/child) then floods its OWN stderr with thousands of engine lines, both
    // corrupting the guest's stream and stalling the build on pipe backpressure. It is a debug aid, so gate it
    // behind the same JT/JTS syscall-tracing flags as the [sys] trace above -- silent by default. The ENOSYS
    // return below is the real, correct behaviour and stays unconditional.
    if (g_trace || g_systrace)
        fprintf(stderr, "[jit] unhandled syscall %llu (a0=%llx a1=%llx) at pc=%llx\n", (unsigned long long)nr,
                (unsigned long long)a0, (unsigned long long)a1, (unsigned long long)G_PC(c));
    G_RET(c) = (uint64_t)(-ENOSYS);
    // Boundary errno translation: every case sets G_RET(c) to a host(macOS) errno on error
    // (-errno, saved e, helper returns, or a macOS E* constant). Map to the Linux errno the guest
    // expects. Skip redirect (sigreturn restored an already-Linux x0 from the signal frame).
    if (!c->redirect) {
        int64_t rv = (int64_t)G_RET(c);
        if (rv < 0 && rv >= -4095) G_RET(c) = (uint64_t)(-(int64_t)m2l_errno((int)(-rv)));
    }
}
