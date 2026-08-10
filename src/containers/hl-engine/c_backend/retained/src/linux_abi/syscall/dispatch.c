// brk arena
static uint64_t brk_lo, brk_cur, brk_hi;
// W3D fork-server prewarm/worker: when set, the guest's exit_group UNWINDS run_guest (sets c->exited
// + c->exit_code) instead of _exit()ing, so the resident engine server survives pre-translating a
// binary into the COW arena and a worker can report its exit code before dying. 0 on every normal
// (standalone) run -> exit_group behaves exactly as before.
int g_noexit;
// W6A item 3: set the first time a guest requests a PROT_EXEC (RWX) anonymous mapping -- i.e. a
// guest with its own in-process JIT (JVM/V8/LuaJIT/.NET/PyPy). Normal guests never set it, so the
// SMC write-fault invalidation path (frontend/x86_64) stays completely inert for the whole existing
// test matrix (g_rwx_guest==0 -> smc_protect()/smc_on_write() are no-ops -> bit-exact).
int g_rwx_guest;
// hl/runtime/os/linux -- service(): the Linux syscall layer (the "kernel" the guest talks to).
// Dispatches the guest syscall number to the host, translating the ABI (errno, struct layouts, flags,
// fd semantics); every path argument is resolved through the container VFS jail.
// The sorted switch below is grouped by syscall category.

/* The descriptor vocabulary, for HL_LINUX_HOST_NULL_DEVICE below and in the
   fragments this file includes. Named here rather than left to the enclosing
   target unit: only the x86-64 one includes it, so relying on that made this
   file compile in one target and not the other. */
#include "../host_fd.h"
#include "../host_sysv.h"
#include "../host_sysv.h"
#include "../host_sysv.h"
#include "../host_sysv.h"
#include "../host_dirent.h" // <dirent.h>, or the Linux dirent shape where the host structure has no d_type
#include <stdlib.h>
#include "../host_proc.h" // times(2): CPU accounting (struct tms is layout-compatible with Linux)
#include "../host_fs.h"   // host struct statfs -> translated to the Linux statfs layout
#include <time.h>         // sysinfo(2) uptime = now - host boot time
#include "../errno.h"
#include "../../host/directory.h"
#include "../../host/process.h"
#include "../../host/system.h" // host memory/boot snapshot feeding sysinfo(2), consistent with /proc/meminfo
// Linux AT_FDCWD(-100) -> host AT_FDCWD; real directory descriptors pass through unchanged.
#define ATFD(value) (((int)(value) == -100) ? AT_FDCWD : (int)(value))
// ================= ptrace(2) — in-hl tracer/tracee coordination ========================
// hl runs each guest PROCESS as its own host process (fork(2) is a real host fork; see proc.c case 220),
// so a guest tracer ptracing a guest tracee is TWO host processes. We CANNOT proxy to the host macOS
// ptrace: macOS ptrace has no Linux semantics and cannot see the tracee's GUEST register file (which
// lives in the tracee's own `struct cpu`). Instead we emulate the ptrace relationship *between* the two
// guest processes over a shared-memory arena (MAP_SHARED|ANON, mmap'd ONCE at engine_global_init BEFORE
// any guest fork, so every descendant guest process inherits the same physical pages). Keyed on guest
// pids (hl already maps guest<->host pids via g_init_hostpid/container_pid). The tracee publishes its
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
static void mq_fd_close(int fd);
static void mq_fd_duplicate(int newfd, int oldfd);

#include "../../core/provider/files.h"
#include "../object.h"
#include "provider_epoll_registry.h"
static ep_provider_watch g_ep_provider_watches[EP_PROVIDER_WATCH_LIMIT];
static uint32_t g_ep_provider_generations[HL_NFD];
static uint32_t g_ep_provider_serial;

static void ep_provider_retire(ep_provider_watch *watch) {
    if (!ep_provider_retire_begin(watch)) return;
    hl_provider_files_unsubscribe(watch->handle, watch, atomic_load(&watch->serial));
    while (atomic_load_explicit(&watch->callbacks, memory_order_acquire) != 0)
        sched_yield();
    ep_provider_retire_finish(watch);
}

static void ep_provider_retire_endpoint(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    uint32_t epoll_generation = g_ep_provider_generations[fd];
    for (uint32_t index = 0; index < EP_PROVIDER_WATCH_LIMIT; ++index) {
        ep_provider_watch *watch = &g_ep_provider_watches[index];
        if (atomic_load_explicit(&watch->state, memory_order_acquire) == EP_PROVIDER_ACTIVE &&
            ((watch->epoll == fd && watch->epoll_generation == epoll_generation) || watch->descriptor == fd))
            ep_provider_retire(watch);
    }
}

/* Object-backed epoll watches.  A typed box object (an inotify watch is the
 * canonical case) exposes readiness only through its object adapter -- it owns
 * host observation and never surfaces a host descriptor the guest epoll kqueue
 * could wait on.  poll()/select() already observe such objects by sampling their
 * readiness on a bounded tick (hl_linux_object_poll); epoll mirrors that exact
 * pattern here so an armed inotify fd is epollable as it is on native Linux.
 * epoll_generation reuses g_ep_provider_generations, which is bumped whenever an
 * epoll or watched fd number is closed, so a reused number never matches. */
#define EP_OBJECT_WATCH_LIMIT 1024u

typedef struct ep_object_watch {
    _Atomic uint32_t active;
    int epoll;
    uint32_t epoll_generation;
    int descriptor;
    uint32_t descriptor_generation;
    uint32_t events; /* raw guest epoll event mask (EPOLLONESHOT etc.) */
    uint32_t interests;
    uint64_t data;
} ep_object_watch;

static ep_object_watch g_ep_object_watches[EP_OBJECT_WATCH_LIMIT];
static uint16_t g_ep_object_count[HL_NFD];

static ep_object_watch *ep_object_find(int epoll, uint32_t epoll_generation, int descriptor,
                                       uint32_t descriptor_generation) {
    for (uint32_t index = 0; index < EP_OBJECT_WATCH_LIMIT; ++index) {
        ep_object_watch *watch = &g_ep_object_watches[index];
        if (atomic_load_explicit(&watch->active, memory_order_acquire) != 0 && watch->epoll == epoll &&
            watch->epoll_generation == epoll_generation && watch->descriptor == descriptor &&
            watch->descriptor_generation == descriptor_generation)
            return watch;
    }
    return NULL;
}

static ep_object_watch *ep_object_alloc(void) {
    for (uint32_t index = 0; index < EP_OBJECT_WATCH_LIMIT; ++index) {
        uint32_t expected = 0;
        if (atomic_compare_exchange_strong_explicit(&g_ep_object_watches[index].active, &expected, 1u,
                                                    memory_order_acq_rel, memory_order_relaxed))
            return &g_ep_object_watches[index];
    }
    return NULL;
}

static void ep_object_free(ep_object_watch *watch) {
    if (watch->epoll >= 0 && watch->epoll < HL_NFD && g_ep_object_count[watch->epoll] > 0)
        g_ep_object_count[watch->epoll]--;
    atomic_store_explicit(&watch->active, 0u, memory_order_release);
}

static void ep_object_retire_endpoint(int fd) {
    if (fd < 0 || fd >= HL_NFD) return;
    for (uint32_t index = 0; index < EP_OBJECT_WATCH_LIMIT; ++index) {
        ep_object_watch *watch = &g_ep_object_watches[index];
        if (atomic_load_explicit(&watch->active, memory_order_acquire) != 0 &&
            (watch->epoll == fd || watch->descriptor == fd))
            ep_object_free(watch);
    }
    g_ep_object_count[fd] = 0;
}

#include "helpers.c"
#include "guest_copy.c"
// Included after the canonical guest-copy helpers and before proc/rare so all
// seccomp user objects, including nested filter pointers, use the same access
// path as the rest of syscall emulation.
#include "../seccomp.c"
#include "sysv.c"
#include "mem.c"
#include "signal.c"
#include "time.c"
static int bound_sentinel_vacate(int target);
#include "io.c"
#include "aio.c"
#include "net.c"
#include "event.c"
#include "misc.h"
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

static void misc_random(void *context, void *output, size_t size) {
    (void)context;
    arc4random_buf(output, size);
}

static ssize_t misc_copy_from(void *context, void *destination, uint64_t source, size_t size) {
    (void)context;
    return guest_copy_from(destination, source, size);
}

static ssize_t misc_copy_to(void *context, uint64_t destination, const void *source, size_t size) {
    (void)context;
    return guest_copy_to(destination, source, size);
}

// g2h-style redirect for non-PIE ET_EXEC pointer args. A non-PIE links at a fixed low vaddr but is biased
// HIGH by load_elf (__PAGEZERO forbids the low 4 GB); an un-relocated pointer baked at the low link vaddr
// (e.g. a global .rodata string handed to open()/write()) still names the low range, where nothing is
// mapped. The real bytes live at addr+g_nonpie_bias in the high-mapped image, so any *pointer* syscall arg
// that lands in [g_nonpie_lo,g_nonpie_hi) must be rebased before the host syscall dereferences it. Inert
// for PIE/static-PIE (g_nonpie_lo==0, the only state the test matrix ever sees) and for any pointer that is
// already high (stack/heap/bss-above-bias) -> byte-identical there. Apply ONLY to pointer positions.
static inline uint64_t nonpie_p(uint64_t a) {
    return nonpie_fold(a); // thread.c owns the one fold; this is its historical name at the syscall seam
}

#include "nonpie_args.h" // the one per-syscall pointer-argument table (service_local + the sentry share it)

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
    struct timespec now;
    if (hl_production_clock_gettime(effective_host_services(), HL_PRODUCTION_CLOCK_REALTIME, &now) != 0) return -EIO;
    *(int64_t *)(tx + 8) = 0;         // offset (us)
    *(int64_t *)(tx + 16) = 0;        // freq (scaled ppm)
    *(int64_t *)(tx + 24) = 16384;    // maxerror (us)
    *(int64_t *)(tx + 32) = 16384;    // esterror (us)
    *(int32_t *)(tx + 40) = 0x0040;   // status = STA_UNSYNC
    *(int64_t *)(tx + 48) = 2;        // constant
    *(int64_t *)(tx + 56) = 1;        // precision (us)
    *(int64_t *)(tx + 64) = 32768000; // tolerance (default)
    *(int64_t *)(tx + 72) = now.tv_sec;
    *(int64_t *)(tx + 80) = now.tv_nsec / 1000;
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

// Mint a pidfd for `pid`. Ask the host for a descriptor that becomes persistently readable when the process
// exits; the macOS backend uses an EVFILT_PROC watch and Linux uses pidfd_open. This is the load-bearing half of
// CLONE_PIDFD (go/rust/glibc-posix_spawn epoll_wait the returned pidfd to reap their compiler child). If the process is
// already gone or EVFILT_PROC can't arm (e.g. a non-child target), fall back to an always-readable /dev/null
// fd so a wait returns immediately instead of blocking forever. Registers the fd->pid map for
// waitid(P_PIDFD)/pidfd_send_signal. Returns -1 only if no fd could be opened at all.
static int pidfd_make(pid_t pid) {
    int watched = hl_host_process_open(pid);
    if (watched >= 0) {
        if (pidfd_register(watched, pid) != 0) { // table full: don't hand back an unresolvable fd
            close(watched);
            errno = EMFILE;
            return -1;
        }
        return watched;
    }
    int fd = open(HL_LINUX_HOST_NULL_DEVICE, O_RDONLY | O_CLOEXEC);
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

static int8_t g_mqfd_queue[HL_NFD]; /* queue index + 1; zero means this fd is not an mqueue descriptor */
static uint16_t g_mqfd_flags[HL_NFD];
static uint8_t g_mqfd_amode[HL_NFD]; /* O_ACCMODE of the open: O_RDONLY(0)/O_WRONLY(1)/O_RDWR(2) */
static uint32_t g_mqfd_group[HL_NFD];
static uint32_t g_mqfd_next_group = 1;
static void mq_maybe_free(int qi);

static int mq_find(const char *name) {
    for (int i = 0; i < MQ_MAXQ; i++)
        if (g_mqq[i].used && !g_mqq[i].unlinked && !strcmp(g_mqq[i].name, name)) return i;
    return -1;
}

static int mq_qof(int fd) {
    return fd >= 0 && fd < HL_NFD && g_mqfd_queue[fd] != 0 ? (int)g_mqfd_queue[fd] - 1 : -1;
}

static int mq_bind(int fd, int qi) {
    if (fd < 0 || fd >= HL_NFD || qi < 0 || qi >= MQ_MAXQ || g_mqfd_queue[fd] != 0) return -1;
    g_mqfd_queue[fd] = (int8_t)(qi + 1);
    g_mqfd_group[fd] = g_mqfd_next_group++;
    if (g_mqfd_next_group == 0) g_mqfd_next_group = 1;
    return 0;
}

static void mq_fd_duplicate(int newfd, int oldfd) {
    int qi = mq_qof(oldfd);
    if (qi < 0 || newfd < 0 || newfd >= HL_NFD || g_mqfd_queue[newfd] != 0) return;
    g_mqfd_queue[newfd] = (int8_t)(qi + 1);
    g_mqfd_flags[newfd] = g_mqfd_flags[oldfd];
    g_mqfd_amode[newfd] = g_mqfd_amode[oldfd]; // a dup shares the open file description's access mode
    g_mqfd_group[newfd] = g_mqfd_group[oldfd];
    g_mqq[qi].refs++;
}

static void mq_fd_close(int fd) {
    int qi = mq_qof(fd);
    if (qi < 0) return;
    g_mqfd_queue[fd] = 0;
    g_mqfd_flags[fd] = 0;
    g_mqfd_amode[fd] = 0;
    g_mqfd_group[fd] = 0;
    g_mqq[qi].refs--;
    mq_maybe_free(qi);
}

// Per-descriptor O_NONBLOCK: report/set the mq_flags of the open file description behind fd. Recorded at
// mq_open time and toggled by mq_getsetattr (the mq equivalent of F_SETFL) so a blocking descriptor and a
// non-blocking one to the same queue behave differently, as on Linux.
static int mq_fd_nonblock(int fd) {
    return mq_qof(fd) >= 0 && (g_mqfd_flags[fd] & MQ_O_NONBLOCK) != 0;
}

static void mq_fd_setnb(int fd, int on) {
    uint32_t group;
    if (mq_qof(fd) < 0) return;
    group = g_mqfd_group[fd];
    for (int i = 0; i < HL_NFD; ++i)
        if (g_mqfd_group[i] == group) g_mqfd_flags[i] = on ? MQ_O_NONBLOCK : 0;
}

// Access-mode enforcement: the kernel checks FMODE_WRITE/FMODE_READ on the descriptor right after the fd
// lookup (before EMSGSIZE), so a send on an O_RDONLY descriptor or a receive on an O_WRONLY one is EBADF.
// O_RDONLY(0)/O_WRONLY(1)/O_RDWR(2) are identical on aarch64 and x86_64.
static int mq_fd_canwrite(int fd) {
    return mq_qof(fd) >= 0 && g_mqfd_amode[fd] != O_RDONLY; // O_WRONLY or O_RDWR
}

static int mq_fd_canread(int fd) {
    return mq_qof(fd) >= 0 && g_mqfd_amode[fd] != O_WRONLY; // O_RDONLY or O_RDWR
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
static int linux_online_cpus(void) {
    // container_online_cpus() (state.c) applies the docker --cpus cap (ceil(NanoCpus/1e9)) on top of the
    // host online count, so sched_getaffinity / the cpu-topology sysfs advertise the container's allotment.
    return container_online_cpus();
}

// Build the "all online CPUs" bitmask into the caller's buffer (CPU i -> bit i, little-endian bytes).
// Current CPU-affinity mask (process-global; default = all online CPUs). sched_setaffinity records the
// guest's chosen mask so sched_getaffinity round-trips it (pin-to-CPU0 then read back), while a fresh
// process still advertises every online CPU so glibc/tcmalloc size their per-CPU tables correctly.
#include "../affinity.h"
static struct hl_linux_affinity g_affinity;

// Lowest CPU id in the current affinity mask. getcpu(2) must return a CPU the task is allowed to run
// on; when the guest has pinned itself to a single CPU via sched_setaffinity, that is the exact value
// LTP getcpu01 expects back. Falls back to CPU 0 for an (impossible) empty mask.
// Back a short synthesized sysfs string with an anonymous temp fd (the same trick proc_open uses for
// the macOS-has-no-/proc case). Returns a readable fd positioned at offset 0, or -1 on error.
static int synth_str_fd(const char *s) {
    char tn[] = "/tmp/.hl-cpuXXXXXX";
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
    hl_linux_affinity_range(buf, n, linux_online_cpus());
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
    // A chroot changes which mount points are reachable. The engine's synthetic /proc belongs to the
    // launch root and must not leak into a new root merely because the guest spells an absolute /proc
    // path. In particular, Chromium's setuid sandbox chroots into an empty directory and verifies that
    // /proc/self/exe can no longer be opened before treating the filesystem sandbox as sealed.
    if (g_chroot[0]) return 0;
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
            if (hl_native_fd_path(dirfd, db, sizeof db) == 0) {
                if (g_rootfs) {
                    char gd[4200];
                    if (guest_from_host_raw(db, gd, sizeof gd) <= 0) {
                        if (n != 0) out[0] = 0;
                        return;
                    }
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
#include "../watch.h"
#include "inotify.c"
/* fs.c handles synthetic /proc/self/fd and /dev/fd opens before the generic bound syscall route.  A
 * projected/typed descriptor must be duplicated through the Linux fd model rather than reopening its
 * native sentinel path; binding.c supplies the allocator after the syscall families are included. */
static int64_t bound_dup_at_least(hl_linux_fd source, int minimum, uint32_t descriptor_flags);
#include "fs.c"
static void bound_mapping_reset(void);
static size_t bound_mapping_watch_capacity(void);
static int bound_mapping_fork_prepare(hl_linux_watch_fork_plan *plan);
static int bound_mapping_fork_complete(hl_linux_watch_fork_plan *plan, int child);
#include "proc.c"
#include "rare.c"
#include "ptrace.c" // bug real ptrace tracer/tracee coordination (uses helpers above + G_* macros)
#include "binding.c"

static void service(struct cpu *c) {
    // Mark this thread as "in a host syscall" for the whole service window (incl. any blocking wait such
    // as pause()/ppoll()/read()): the fault-class-signal handlers use it to tell an external kill of
    // SIGSEGV/ILL/FPE/... from a genuine engine fault (see g_in_service in os/linux/signal.c). Cleared on
    // EVERY exit path below, so the ptrace/untrusted routes must not early-return past the clear.
    g_in_service = 1;
    __atomic_store_n(&c->in_service, 1, __ATOMIC_SEQ_CST);
    filemap_replay();
    // Close the signal-vs-syscall-entry race. The signaler publishes tpending
    // before reading in_service; after publishing in_service, recheck it here
    // before entering a potentially blocking host call.
    if (cpu_has_actionable_tsig(c)) {
        // Do not execute or advance past the syscall. The dispatcher delivers
        // the pending handler, and sigreturn resumes at this SVC exactly as a
        // Linux signal noticed before syscall entry would.
        c->redirect = 1;
        __atomic_store_n(&c->in_service, 0, __ATOMIC_SEQ_CST);
        g_in_service = 0;
        return;
    }
    // Preserve the guest's first syscall-argument register so a transparent SA_RESTART restart (see
    // syscall_should_restart) can re-execute the SVC with the ORIGINAL arg. On aarch64 arg0 and the return
    // value share x0, so a handler-then-restart would otherwise feed the just-written result back as arg0.
    uint64_t _svc_arg0 = G_A0(c);
    // On x86-64 the syscall NUMBER and the return value share RAX (G_RET), so once the interrupted call wrote
    // its -EINTR result the number is gone -- a transparent restart would re-issue `syscall` with -EINTR as
    // the number. Preserve the entry number register so the SA_RESTART restart re-executes the SAME syscall.
    uint64_t _svc_nrreg = G_RET(c);
    g_syscall_restart = 0;
    // Preserve the canonical syscall number across dispatch. x86 aliases the syscall-number and return-value
    // register (RAX), so G_NR(c) after service_local() would translate the result instead of identifying the
    // completed syscall. Post-dispatch FD publication and restart bookkeeping must never depend on tracing.
    uint64_t _rnr = G_NR(c);
    // seccomp gate: run the guest's installed cBPF filter(s) / STRICT policy against this syscall BEFORE it
    // is routed anywhere. On an intercepted syscall (ERRNO/TRAP/TRACE/KILL/strict-violation) the result is
    // already set in G_RET / a signal is queued / the process is killed, so we must NOT service it. Inert
    // (one predicted-not-taken load) until a guest installs a filter. Runs on the RAW guest register state,
    // before x86 legacy-syscall normalization, so the filter sees the number/args the guest actually issued.
    if (__builtin_expect(seccomp_gate(c) != 0, 0)) {
#if HL_ENABLE_LOGGING
        if (g_systrace)
            fprintf(stderr, "[ret pid=%d] %llu -> %lld (seccomp)\n", (int)getpid(), (unsigned long long)_rnr,
                    (long long)(int64_t)G_RET(c));
#endif
        __atomic_store_n(&c->in_service, 0, __ATOMIC_SEQ_CST);
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
    if (!g_untrusted && (int64_t)G_RET(c) >= 0) {
        int fd = (int)G_RET(c);
        switch (_rnr) {
        case 19:  /* eventfd2 */
        case 20:  /* epoll_create1 */
        case 23:  /* dup */
        case 26:  /* inotify_init1 */
        case 56:  /* openat */
        case 74:  /* signalfd4 */
        case 85:  /* timerfd_create */
        case 198: /* socket */
        case 202: /* accept */
        case 242: /* accept4 */
        case 437: /* openat2 */
            if (_rnr == 85) {
                timerfd_object_assign(fd);
                if (fd >= 0 && fd < HL_NFD) g_tfd_nb[fd] = (G_A1(c) & 0x800) != 0;
            }
            if (_rnr == 26) inotify_object_assign(fd);
            (void)proc_fdvis_publish_native_fd(fd);
            break;
        case 24: /* dup3: result is the requested target descriptor */
            (void)proc_fdvis_publish_native_fd((int)G_A1(c));
            break;
        default: break;
        }
    }
    filemap_replay();
    // A restart-redirect was requested (SA_RESTART handler pending on an interrupted blocking syscall): the
    // syscall re-executes at the same SVC once the handler returns, so restore the arg0/return-aliased
    // register the dispatch overwrote with the (discarded) EINTR result. The negative EINTR already made the
    // fd-publish block above a no-op, so this runs strictly after it.
    if (g_syscall_restart && c->redirect) {
        G_A0(c) = _svc_arg0;
#if defined(HL_GUEST_SIGACTION_HAS_RESTORER)
        // x86 pre-advances rip past the `syscall` instruction (0F 05, 2 bytes) at emit time, so `redirect`
        // alone -- which only suppresses aarch64's post-service pc+=4 -- cannot re-execute it. Rewind rip to
        // the syscall instruction so the pending SA_RESTART handler's sigframe saves that pc and sigreturn
        // resumes ON it, transparently restarting the interrupted read()/waitpid() (eintr_restart_read/wait,
        // sarestart). aarch64 needs no rewind: leaving pc on the SVC is exactly what skipping the +4 does.
        G_PC(c) -= 2;
        // Restore RAX to the original syscall number (it shares the register with the just-written -EINTR
        // return), so the re-executed `syscall` re-issues the same call rather than a garbage number.
        G_RET(c) = _svc_nrreg;
#endif
    }
#if HL_ENABLE_LOGGING
    if (g_systrace)
        fprintf(stderr, "[ret pid=%d] %llu -> %lld\n", (int)getpid(), (unsigned long long)_rnr,
                (long long)(int64_t)G_RET(c));
#endif
    __atomic_store_n(&c->in_service, 0, __ATOMIC_SEQ_CST);
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
    HL_LOGF(&g_jit_log, HL_LOG_TAG_SYSCALL, "nr=%llu a0=%#llx a1=%#llx a2=%#llx", (unsigned long long)nr,
            (unsigned long long)a0, (unsigned long long)a1, (unsigned long long)a2);
#if HL_ENABLE_LOGGING
    if (g_trace || g_systrace)
        fprintf(stderr, "[sys pid=%d] %llu (%llx,%llx,%llx,%llx,%llx,%llx)\n", (int)getpid(), (unsigned long long)nr,
                (unsigned long long)a0, (unsigned long long)a1, (unsigned long long)a2, (unsigned long long)a3,
                (unsigned long long)a4, (unsigned long long)a5);
#endif
    // --- non-PIE ET_EXEC pointer-arg redirect (g2h) --------------------------------------------------
    // The table lives in nonpie_args.h -- ONE list, shared with the sentry trust boundary, which applies it
    // restricted to what it forwards. Runs BEFORE the resolution-bump switch below, which itself
    // dereferences a2 (open_how*) for openat2. Inert unless g_nonpie_lo is set (ET_EXEC only).
    if (g_nonpie_lo) {
        uint64_t reb[6] = {a0, a1, a2, a3, a4, a5};
        nonpie_rebase_args(nr, reb);
        // This caller hands the iovec array straight to a host readv/writev, so its ENTRIES must be folded
        // too; the table folds only the array base.
        if (nonpie_iov_carrier(nr)) nonpie_rebase_iov(&reb[1], reb[2]);
        a0 = reb[0];
        a1 = reb[1];
        a2 = reb[2];
        a3 = reb[3];
        a4 = reb[4];
        a5 = reb[5];
    }
    // daemon-write coherence: notice a daemon-side write into this container's fs (docker cp /
    // exec-spawn /etc rewrites) and drop the path/metadata caches BEFORE any handler below can consult
    // them -- one shared-page atomic load per syscall (see hl_fdcache_generation_poll).
    hl_fdcache_generation_poll();
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
        hl_fdcache_resolution_bump();
        break;
    case 56: // openat: a2 = Linux flags. O_CREAT (0x40) adds a name. In OVERLAY mode a write-open
             // (O_WRONLY/O_RDWR, a2&3) copies the file lower->upper, RELOCATING its resolved host path
             // -- so it must invalidate too, or a cached lower path goes stale (flat rootfs: no copy-up).
        if ((a2 & 0x40) || (g_nlower && (a2 & 3))) hl_fdcache_resolution_bump();
        break;
    case 437: { // openat2: flags live in open_how.flags (a2 -> struct open_how *), before its case rewrites a2
        // a2 is a raw guest pointer to struct open_how; peek how[0] (flags) only if it is actually mapped, so
        // a bad pointer doesn't fault the engine in this pre-dispatch cache-invalidation probe. If it is
        // unmapped we simply skip the resolution bump -- the real openat2 handler (svc_fs) returns -EFAULT below.
        uint64_t flags;
        if (guest_copy_from(&flags, a2, sizeof flags) == sizeof flags && ((flags & 0x40) || (g_nlower && (flags & 3))))
            hl_fdcache_resolution_bump();
        break;
    }
    default: break;
    }
    if (bound_route(c, nr, a0, a1, a2, a3, a4)) return;
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
    {
        const uint64_t arguments[6] = {a0, a1, a2, a3, a4, a5};
        int64_t result = 0;
        // Host memory/uptime/load snapshot for sysinfo(2). Read from the SAME host backend vfs.c feeds
        // /proc/meminfo and /proc/uptime from, so the three sources report the same machine size and uptime.
        hl_host_system_info hsi;
        uint64_t host_total = 0, host_free = 0, uptime_s = 0, loads[3] = {0, 0, 0};
        if (hl_host_system_read(&hsi, NULL, 0)) {
            host_total = hsi.memory_total;
            host_free = hsi.memory_free;
            time_t now = time(NULL);
            if ((uint64_t)now > hsi.boot_time_seconds) uptime_s = (uint64_t)now - hsi.boot_time_seconds;
        }
        double la[3] = {0, 0, 0};
        if (getloadavg(la, 3) == 3)
            for (int li = 0; li < 3; li++)
                loads[li] = (uint64_t)(la[li] * 65536.0);
        hl_linux_misc_context misc = {
            .hostname = g_hostname,
            .hostname_capacity = sizeof(g_hostname),
            .memory_limit = g_mem_max,
            .memory_used = atomic_load(&g_mem_charged),
            .host_memory_total = host_total,
            .host_memory_free = host_free,
            .uptime_seconds = uptime_s,
            .loads = {loads[0], loads[1], loads[2]},
            .machine = G_UNAME_MACHINE,
            .copy_from = misc_copy_from,
            .copy_to = misc_copy_to,
            .random = misc_random,
            .callback_context = NULL,
        };
        if (hl_linux_misc_dispatch(&misc, nr, arguments, &result)) {
            G_RET(c) = (uint64_t)result;
            (void)svc_done(c);
            return;
        }
    }
    if (svc_rare(c, nr, a0, a1, a2, a3, a4, a5)) return;
    // ===================== unhandled =====================
    // Every Linux syscall is now owned by one of the svc_*() family modules above; reaching here means no
    // family claimed this number -> ENOSYS (the guest sees ENOSYS and falls back, exactly as on a real kernel
    // that lacks the syscall). this report used to be UNCONDITIONAL and went to fd 2 -- i.e. the
    // GUEST's stderr, not the engine's. A guest that pokes an unimplemented number in a hot loop (the arm64 Go
    // toolchain does, per goroutine/child) then floods its OWN stderr with thousands of engine lines, both
    // corrupting the guest's stream and stalling the build on pipe backpressure. It is a debug aid, so gate it
    // behind the same syscall-tracing flags as the [sys] trace above -- silent by default. The ENOSYS
    // return below is the real, correct behaviour and stays unconditional.
#if HL_ENABLE_LOGGING
    if (g_trace || g_systrace)
        fprintf(stderr, "[jit] unhandled syscall %llu (a0=%llx a1=%llx) at pc=%llx\n", (unsigned long long)nr,
                (unsigned long long)a0, (unsigned long long)a1, (unsigned long long)G_PC(c));
#endif
    G_RET(c) = (uint64_t)(-ENOSYS);
    // Boundary errno translation: every case sets G_RET(c) to a host(macOS) errno on error
    // (-errno, saved e, helper returns, or a macOS E* constant). Map to the Linux errno the guest
    // expects. Skip redirect (sigreturn restored an already-Linux x0 from the signal frame).
    if (!c->redirect) {
        int64_t rv = (int64_t)G_RET(c);
        if (rv < 0 && rv >= -4095) G_RET(c) = (uint64_t)(-(int64_t)hl_linux_errno_from_macos((int)(-rv)));
    }
}
