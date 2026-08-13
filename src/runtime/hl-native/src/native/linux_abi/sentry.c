// hl/linux_abi -- sentry process-split: the untrusted-guest isolation trust boundary.
//
// THREAT MODEL. The whole guest->host authority crossing is run_guest()->service(c). A malicious
// translated guest can forge any register state at a syscall, so service() is the one place that
// turns guest intent into real host effects. For UNTRUSTED images we split that authority across a
// process boundary:
//
//   WORKER  (this process)  -- runs the JIT + translated (untrusted) guest. Keeps ONLY compute/
//                              memory authority: brk, anonymous mmap, futex, clocks, the inline
//                              fast-paths. Holds no real fs/net fds. Under HL_SANDBOX it is also
//                              wrapped in a deny-default macOS Seatbelt profile, so even a fully
//                              hijacked worker cannot reach the host fs/net directly -- only the ring.
//   SENTRY  (forked child)  -- holds host fs/net/proc authority. It owns the real fds and runs the
//                              real service_local() (path-jail, /proc synth, overlay, ...) on the host.
//
// They communicate over a POOL of shared-memory SPSC mailboxes (1-deep rings; guest syscalls are
// synchronous so depth 1 is what each gets exercised at). A guest thread is a host pthread and every
// host thread drives this path, so a SINGLE ring would have multiple producers and stall a threaded/
// forking guest -- instead each worker thread claims its own ring from the pool and the sentry runs one
// servicer thread per ring (see the per-context-ring section + roadmap item 1 below).
// The worker marshals {normalized syscall registers, inline buffer} into its ring; the sentry
// executes and returns {result/errno, out-buffer}. Guest memory is NOT shared -- only the marshaled
// bytes cross -- which IS the isolation: the sentry never dereferences a worker-controlled pointer
// into guest memory, and the worker never sees a real host fd (an openat result is an integer that
// is only meaningful inside the sentry; the subsequent read/write/close on it are ALSO forwarded, so
// the fd lives and dies entirely in the sentry).
//
// PORTABILITY. This file is #included into BOTH target unity TUs (linux_x86_64.c, linux_aarch64.c),
// next to service.c. It uses only the frontend-agnostic G_* ABI macros (G_NR/G_A0..G_A5/G_RET/
// G_NORMALIZE), so the marshaling is identical on either guest arch. The one register without an
// lvalue accessor is the raw syscall-number register (G_NR maps it through the number component on x86), so we
// add G_RAWNR, discriminated by G_PROF_EXTRA -- the same x86-vs-aarch64 switch service.c already uses.
//
// Gate: everything here is dormant unless the HL_UNTRUSTED option is set. With the gate off
// (the default + the whole matrix) service() never calls syscall_route(); this TU contributes only a
// statically-predicted-not-taken branch. Byte-identical to baseline by construction.

#include <sched.h>
#include "host_wait.h"
#include <stdatomic.h>

#include "memory_arena.h"
#include "forwarded.h"

static int g_untrusted = 0;      // HL_UNTRUSTED: route fs/net/proc syscalls through the sentry
static int g_sentry_sandbox = 0; // HL_SANDBOX: wrap the worker in a deny-default Seatbelt profile

// The raw syscall-number register. G_NR is not an lvalue on x86 (it maps the guest rax through the
// canonical-number table), so a synthetic CPU must project a canonical syscall back into the guest ISA
// before service_local normalizes it again.
#ifdef G_PROF_EXTRA
#define G_RAWNR(c) ((c)->r[0]) // x86-64 guest: rax
#else
#define G_RAWNR(c) ((c)->x[8]) // aarch64 guest: x8
#endif

static int sentry_cpu_set_canonical(struct cpu *cpu, uint64_t canonical) {
#ifdef G_PROF_EXTRA
    uint64_t raw = hl_linux_syscall_guest_number(HL_LINUX_GUEST_X86_64, canonical);
    if (raw == UINT64_MAX) return 0;
    G_RAWNR(cpu) = raw;
#else
    G_RAWNR(cpu) = canonical;
#endif
    return 1;
}

// Inline payload per request: write data, read results, path strings. 1 MiB covers typical libc
// block sizes; larger transfers degrade to (legal) short reads/writes and the guest libc loops.
#define SENTRY_BUFSZ (1u << 20)

// Two-buffer (stat-family) layout inside buf[]: the in-path occupies [0, SENTRY_PATHCAP); the out
// struct lands at SENTRY_PATHCAP so the path and the struct never alias. The guest struct stat is
// per-arch (x86-64 = 144B, aarch64 = 128B; same x86-vs-aarch64 discriminator service.c uses for
// G_RAWNR); struct statx is arch-independent (256B).
#define SENTRY_PATHCAP 4096u
#ifdef G_PROF_EXTRA
#define SENTRY_STATSZ 144u // x86-64 guest struct stat
#else
#define SENTRY_STATSZ 128u // aarch64 guest struct stat
#endif
#define SENTRY_STATXSZ 256u
// readv/writev: cap the segment count we flatten (room for the iovec[] header before the data; the
// guest libc loops on a short scatter/gather just as it does on a short read/write).
#define SENTRY_IOVMAX 1024u

// Socket marshaling windows. The send/recv DATA payload lives in [0, SENTRY_DATACAP) just like
// read/write; the small socket-control buffers (sockaddr, the in/out socklen_t, optval) live in the
// buf[] TAIL so a sendto/recvfrom's data and its address never alias. A guest sockaddr is at most
// sizeof(struct sockaddr_storage)==128, but we give it 256B of slack; optval is tiny in practice so a
// 4KiB window covers every real socket option (a larger getsockopt cap is clamped, like the 1MiB data
// cap -- the guest libc never asks for more). All four windows fit below SENTRY_BUFSZ.
#define SENTRY_DATACAP (SENTRY_BUFSZ - 8192u)                // sendto/recvfrom payload window [0,DATACAP)
#define SENTRY_SADDRCAP 256u                                 // sockaddr in/out window size (>= sockaddr_storage)
#define SENTRY_SADDR_OFF SENTRY_DATACAP                      // sockaddr in/out window offset
#define SENTRY_SLEN_OFF (SENTRY_SADDR_OFF + SENTRY_SADDRCAP) // socklen_t in/out (addrlen / optlen) window
#define SENTRY_OPT_OFF (SENTRY_SLEN_OFF + 64u)               // setsockopt/getsockopt optval window
#define SENTRY_OPTCAP 4096u                                  // optval cap (real socket options are tiny)

// sendmsg/recvmsg (item 2). The guest msghdr GRAPH (msghdr + msg_name + scatter/gather msg_iov + the
// msg_control cmsg buffer) is flattened into the ring: the 56-byte msghdr COPY lives at [0,64); its
// msg_iov is a `struct iovec[]` at MSGIOV_OFF whose iov_base fields hold buf-relative OFFSETS, followed
// by the gathered (send) / reserved (recv) data; msg_name reuses the sockaddr tail window and msg_control
// reuses the optval tail window (a sendmsg never also does getsockopt, so the windows may alias per-op).
// The sentry rebases every offset to a ring pointer before service_local() runs the real Linux<->macOS
// translating sendmsg/recvmsg, so no guest pointer crosses. SCM_RIGHTS "just works": a guest fd in the
// control buffer is ALREADY a sentry-owned fd (every openat/socket/accept returns a sentry fd), so the
// marshaled cmsg carries fd integers the sentry can use verbatim -- no fd translation needed.
#define SENTRY_MSGHDR_SZ 56u                // Linux LP64 struct msghdr (both guest arches identical)
#define SENTRY_MSGIOV_OFF 64u               // iovec[] header start (after the 56B msghdr copy, aligned)
#define SENTRY_MSGNAME_OFF SENTRY_SADDR_OFF // msg_name window (tail; reuses the sockaddr window)
#define SENTRY_MSGCTL_OFF SENTRY_OPT_OFF    // msg_control window (tail; reuses the optval window)
#define SENTRY_MSGCTLCAP SENTRY_OPTCAP      // control buffer cap (real SCM_RIGHTS payloads are tiny)

// Multiplexing windows (item 3): poll/ppoll pollfd array at buf[0] (8B/entry) + its timeout timespec in
// the sockaddr tail window; pselect's three fd_sets at 0/128/256 + timeout at 384 (each fd_set <=128B);
// epoll_pwait out-events at buf[0] (SENTRY_EPEV_SZ/entry). All sentry-owned fds, so the blocking call MUST run in
// the sentry (the fd lives there). fcntl flock / ioctl arg in/out windows reuse buf[0].
#define SENTRY_PSEL_RD 0u
#define SENTRY_PSEL_WR 128u
#define SENTRY_PSEL_EX 256u
#define SENTRY_PSEL_TMO 384u
#define SENTRY_POLL_TMO SENTRY_SADDR_OFF // ppoll timeout timespec (tail; clear of the pollfd array)
// struct epoll_event is per-arch: x86-64 forces __attribute__((packed)) -> 12 bytes (data@4); aarch64/
// asm-generic leaves it naturally aligned -> 16 bytes (4 bytes pad, data@8). Same x86-vs-aarch64
// discriminator (G_PROF_EXTRA) the rest of this file uses for G_RAWNR / SENTRY_STATSZ. We only marshal the
// struct as an opaque blob (by SIZE) across the ring; the `data`-field offset is interpreted inside
// service_local() (service.c's per-arch G_EPEV_DOFF), so the SIZE/stride is all the boundary needs here.
#ifdef G_PROF_EXTRA
#define SENTRY_EPEV_SZ 12u // x86-64 guest: packed struct epoll_event {u32 events; u64 data@4}
#else
#define SENTRY_EPEV_SZ 16u // aarch64 guest: aligned struct epoll_event {u32 events; pad; u64 data@8}
#endif
#define SENTRY_IOCTLCAP 256u // ioctl arg in/out window (winsize/int/termios all fit)
#define SENTRY_FLOCKSZ 32u   // Linux struct flock (fcntl F_GETLK/SETLK/SETLKW)

// Worker<->sentry fd passing (item 3). A sentry-owned fd that a LOCAL worker syscall must touch (a
// file-backed mmap's fd) is lent to the worker via SCM_RIGHTS over a per-ring AF_UNIX control socketpair:
// the worker maps it locally, then drops the borrowed fd -- so the worker's fds stay virtual and memory
// authority stays worker-side. Encoded as a sentinel in `rawnr` so the lend rides the SAME ring round-trip.
#define SENTRY_OP_FDPASS 0xFFFFFFFEu
// Per-process virtual fd table control ops (rawnr sentinels, like FDPASS -- not real syscalls). FORK tells
// the sentry to give a freshly forked child its OWN fd table as a dup-COPY of the parent's; EXIT releases a
// worker process's table (closes its owned real fds) at process exit. Both ride the SAME ring round-trip.
#define SENTRY_OP_FORK 0xFFFFFFFDu // a[0]=prepared snapshot, a[1]=child wpid -> install the copied table
#define SENTRY_OP_EXIT 0xFFFFFFFCu // R->wpid -> free that worker process's fd table
#define SENTRY_OP_EXEC 0xFFFFFFFBu // R->wpid -> close+drop every FD_CLOEXEC virtual fd (guest execve sweep)
#define SENTRY_OP_REAP 0xFFFFFFFAu // a[0]=reaped child wpid -> release a child killed before guest exit cleanup
// Reverse fd adoption (the FDPASS lend, worker->sentry): the worker opened a real fd LOCALLY (a
// per-process synthetic /proc file whose truth lives only in the worker -- see sentry_worker_proc_leaf)
// and hands it over SCM_RIGHTS on this ring's control socketpair; the sentry installs it into the
// worker's virtual fd table so every later read/lseek/close forwards exactly like a sentry-opened fd.
#define SENTRY_OP_ADOPT 0xFFFFFFF9u          // a[0]=cloexec; ctl-socket carries the fd -> ret = the new virtual fd
#define SENTRY_OP_THREAD_EXIT 0xFFFFFFF8u    // release a CLOSE_RANGE_UNSHARE private table for this thread token
#define SENTRY_OP_FORK_PREPARE 0xFFFFFFF7u   // clone caller table now -> positive opaque snapshot id
#define SENTRY_OP_FORK_CANCEL 0xFFFFFFF6u    // a[0]=snapshot id -> release a failed fork's prepared table
#define SENTRY_OP_THREAD_PREPARE 0xFFFFFFF5u // a[0]=child token -> retain caller's exact table before pthread
#define SENTRY_OP_THREAD_CANCEL 0xFFFFFFF4u  // a[0]=unused child token -> release its prepared binding
#define SENTRY_OP_BIND 0xFFFFFFF3u           // resolve/create caller binding before an irreversible local operation

// The guest passes LINUX flag values, but this engine is a macOS binary whose <fcntl.h> O_CLOEXEC differs
// (0x1000000 vs Linux 0x80000). Match the guest's own O_CLOEXEC / SOCK_CLOEXEC / EFD_CLOEXEC /
// EPOLL_CLOEXEC (all == 0x80000 on Linux) when reading a guest syscall's cloexec-request bit.
#define LX_O_CLOEXEC 0x80000u

// ------------------------------------------------------------------ per-context ring pool
// THE SCALING FIX. A single SPSC ring is single-producer: but a guest thread is a HOST pthread
// (os/linux/thread.c spawn_thread -> pthread_create) and EVERY host thread drives run_guest -> service
// -> syscall_route. So a multi-threaded (or forking) guest has SEVERAL host threads all marshaling onto
// one mailbox -- two producers corrupt the ping-pong and the 2nd worker stalls (busybox `sh` stalls at
// its first clone). The fix: a POOL of SENTRY_NRINGS independent rings, one claimed per worker thread,
// serviced by SENTRY_NRINGS sentry THREADS (one per ring) inside the SINGLE sentry process. It must be
// one process (threads, not forked children): the sentry owns the real fds and the guest's fd table is
// SHARED across its threads, so a socket opened while servicing ring A must be usable when servicing
// ring B -- only threads in one process share that fd table. Each worker thread claims a ring once
// (monotonic counter, mod N); at <=N concurrent threads every thread owns a private ring (zero
// contention); beyond N, overflow threads serialize on a per-ring producer lock (`busy`) -- still
// correct, just sharing a lane. (Follow-up: size the pool / hash by tid; wire execve/clone(fork) so a
// forked guest PROCESS gets its own worker registered with the sentry.)
// 8 lanes deadlocked a real .NET SDK build: a forwarded BLOCKING wait (epoll_pwait/ppoll/read on a quiet
// descriptor) parks its servicer thread for as long as the guest thread would block, so every lane can be
// legitimately pinned by one parked waiter. dotnet/MSBuild runs several guest processes x threadpools of
// parked waiters against the ONE shared sentry; once all lanes were pinned, every additional thread fell
// to the shared-lane fallback and spun on a lane whose current request never completes -> whole-container
// hang. 64 lanes keeps every simultaneously-blocked guest thread on its OWN lane for real SDK workloads;
// the shared-lane fallback remains only as an extreme overflow. The idle cost stays bounded because the
// servicer loop backs off to a real sleep once a lane has been quiet for a while (see sentry_ring_loop).
#define SENTRY_NRINGS 64

// The shared-memory mailbox. `turn` is the ownership token: 0 => worker fills a request, 1 => sentry
// executes it. Strict ping-pong (no third state) => deadlock-free and, with release/acquire on turn,
// torn-message-free (all field writes happen-before the token flip the peer acquires).
struct sentry_ring {
    _Atomic uint32_t turn;  // 0 = worker owns (build request), 1 = sentry owns (execute)
    _Atomic uint32_t busy;  // worker-side producer lock: held across one round-trip (uncontended at <=N threads)
    _Atomic uint32_t owner; // ring-pool free-list: 0 = free lane, else the owning worker thread's token
    _Atomic uint64_t request;
    _Atomic uint64_t response;
    // request: the post-normalize syscall registers (frontend-agnostic via G_RAWNR / G_A0..G_A5)
    uint32_t wpid;         // stamping worker PROCESS pid: selects this guest's per-process virtual fd table (P1/P2)
    uint32_t wtid;         // stable worker-thread token: selects a CLOSE_RANGE_UNSHARE private fd-table copy
    uint32_t inherit_wtid; // reserved for wire compatibility; prepared thread bindings no longer defer inheritance
    uint64_t rawnr;        // raw syscall-number register (so the sentry's G_NR re-derives the canonical nr)
    uint64_t a[6];         // a0..a5 (G_A0..G_A5)
    // Generalized pointer marshaling: redir[i] is the byte offset within buf[] that arg i is redirected
    // to (or -1 to leave the register untouched). The sentry rebases a[i] -> buf+redir[i] AFTER bounds-
    // checking the offset, so service_local() only ever dereferences ring memory -- never a worker-
    // supplied guest pointer. Worker stages inputs (paths, write payloads, gathered iovec data) into
    // buf[] before the round-trip and copies outputs (read bytes, stat structs, scattered iovec data)
    // back into guest memory after it -- guest pointers stay entirely on the worker side.
    int32_t redir[6];
    // iovec ops (readv/writev): a1's redirected region is a `struct iovec[iovn]` whose iov_base fields
    // hold buf-relative OFFSETS (not pointers). The sentry bounds-checks each {offset,len} and rebases
    // iov_base to buf+offset, so even a hijacked worker cannot aim the scatter/gather outside the ring.
    uint32_t iovn;
    uint32_t inlen; // informational: input bytes staged in buf[] (measurement)
    // response
    _Atomic int64_t ret;      // syscall return value, or -errno
    _Atomic uint64_t nserved; // sentry-maintained request counter
    uint8_t buf[SENTRY_BUFSZ];
};

// The shared region: one teardown flag, one ring-claim counter (in shared memory so forked-worker
// PROCESSES also draw distinct rings from the pool), and the ring pool itself.
struct sentry_shm {
    _Atomic uint32_t quit;  // worker sets at teardown -> every sentry servicer thread _exit()s the process
    _Atomic uint32_t claim; // monotonic ring-claim counter (shared across worker threads AND forked workers)
    struct sentry_ring ring[SENTRY_NRINGS];
};

static struct sentry_shm *g_shm;
static pid_t g_sentry_pid;
static int g_ctl[SENTRY_NRINGS][2];           // per-ring AF_UNIX control socketpair: [.][0]=worker end, [.][1]=sentry
static pid_t g_worker_pid;                    // this worker process's pid (changes in a forked-child worker)
static pid_t g_sentry_owner_pid;              // ONLY the process that forked the sentry may signal-quit + reap it
static _Atomic int g_guest_children;          // live guest-forked child WORKER processes owned by THIS worker proc
static _Atomic int g_worker_threads;          // live guest threads in this worker; the initial thread contributes one
static _Atomic int g_sentry_process_released; // child process table has been released exactly once
static __thread int t_ring = -1;              // this worker thread's claimed ring index (claimed lazily on first use)
static __thread uint32_t t_token = 0;         // this worker thread's unique nonzero ring-ownership token
static int64_t sentry_ctl_op(uint32_t op, uint64_t a0, uint64_t a1);
static void ring_release(void);

#include "sentry_binding.h"
#include "sentry_start.h"
#include "sentry_token.h"
#include "sentry_pipe.h"
#include "sentry_authority.h"

#define SENTRY_THREAD_STARTS HL_SENTRY_BINDING_CAPACITY
static struct hl_sentry_start g_thread_start[SENTRY_THREAD_STARTS];
static pthread_mutex_t g_thread_start_lock = PTHREAD_MUTEX_INITIALIZER;

static int sentry_thread_prepare(struct cpu *child) {
    if (!g_untrusted) return 0;
    pthread_mutex_lock(&g_thread_start_lock);
    uint32_t token = hl_sentry_token_next(&g_shm->claim);
    int reserved = hl_sentry_start_reserve(g_thread_start, SENTRY_THREAD_STARTS, child, token);
    if (reserved == 0 && sentry_ctl_op(SENTRY_OP_THREAD_PREPARE, token, 0) < 0) {
        uint32_t unused = 0;
        hl_sentry_start_take(g_thread_start, SENTRY_THREAD_STARTS, child, &unused);
        reserved = -EAGAIN;
    }
    if (reserved == 0) atomic_fetch_add(&g_worker_threads, 1);
    pthread_mutex_unlock(&g_thread_start_lock);
    return reserved;
}

static void sentry_thread_cancel(struct cpu *child) {
    if (!g_untrusted) return;
    pthread_mutex_lock(&g_thread_start_lock);
    uint32_t token = 0;
    if (hl_sentry_start_take(g_thread_start, SENTRY_THREAD_STARTS, child, &token) == 0) {
        sentry_ctl_op(SENTRY_OP_THREAD_CANCEL, token, 0);
        atomic_fetch_sub(&g_worker_threads, 1);
    }
    pthread_mutex_unlock(&g_thread_start_lock);
}

static void sentry_thread_enter(struct cpu *child) {
    if (!g_untrusted) return;
    pthread_mutex_lock(&g_thread_start_lock);
    uint32_t token = 0;
    if (hl_sentry_start_take(g_thread_start, SENTRY_THREAD_STARTS, child, &token) == 0) { t_token = token; }
    pthread_mutex_unlock(&g_thread_start_lock);
}

static void sentry_thread_leave(void) {
    if (!g_untrusted || t_token == 0) return;
    sentry_ctl_op(SENTRY_OP_THREAD_EXIT, 0, 0);
    ring_release();
    atomic_fetch_sub(&g_worker_threads, 1);
}

// Claim (once per worker thread) a ring from the pool. A reclaim-aware free-list: scan for a lane whose
// `owner` is 0 and CAS-claim it with this thread's unique token; at <=N concurrent threads every thread
// wins a private lane (zero contention). Beyond N (all lanes owned) overflow threads SHARE a lane keyed by
// token, serialized by the ring's `busy` producer lock (correctness preserved). ring_release() frees a
// lane on thread/process exit so a forking guest doesn't permanently exhaust the pool.
static struct sentry_ring *ring_for_thread(void) {
    if (t_ring < 0) {
        if (!t_token) // unique nonzero token from the shared claim counter (distinct across threads AND forked workers)
            t_token = hl_sentry_token_next(&g_shm->claim);
        for (int i = 0; i < SENTRY_NRINGS; i++) {
            uint32_t expect = 0;
            if (atomic_compare_exchange_strong_explicit(&g_shm->ring[i].owner, &expect, t_token, memory_order_acq_rel,
                                                        memory_order_relaxed)) {
                t_ring = i;
                return &g_shm->ring[i];
            }
        }
        t_ring = (int)(t_token % SENTRY_NRINGS); // pool full: share a lane (busy-serialized)
    }
    return &g_shm->ring[t_ring];
}

// Release this thread's lane back to the pool (thread/process exit). CAS owner==t_token->0 so a SHARED
// (non-owned) lane is left untouched -- we only free a lane we actually own.
static void ring_release(void) {
    if (t_ring >= 0) {
        uint32_t mine = t_token;
        atomic_compare_exchange_strong_explicit(&g_shm->ring[t_ring].owner, &mine, 0, memory_order_acq_rel,
                                                memory_order_relaxed);
        t_ring = -1;
    }
    // Release is terminal for this guest thread.  In particular, exit(93) unwinds through run_guest and
    // the pthread trampoline; clearing the token keeps its later sentry_thread_leave() from publishing a
    // second THREAD_EXIT and decrementing g_worker_threads twice.
    t_token = 0;
}

// A worker that just forked itself (guest clone/clone3 without CLONE_THREAD) calls this in the CHILD: the
// child is a real process running the JIT, but it inherited the parent's ring lane + sentry-ownership +
// child bookkeeping. Adopt a fresh identity so the child draws its OWN lane (next forwarded syscall),
// never tears down the SHARED sentry, and tracks only its own children. NB: do NOT ring_release() here --
// the inherited owner token still belongs to the PARENT's still-live lane.
static void sentry_fork_child(void) {
    g_worker_pid = getpid(); // != g_sentry_owner_pid now -> sentry_shutdown() becomes a no-op in this child
    t_ring = -1;             // drop the inherited lane index; claim a fresh one lazily
    t_token = 0;             // mint a fresh ownership token on the next claim
    g_guest_children = 0;    // the child starts with no children of its own
    g_worker_threads = 1;    // only the calling thread survives fork()
    g_sentry_process_released = 0;
    memset(g_thread_start, 0, sizeof g_thread_start);
    pthread_mutex_init(&g_thread_start_lock, NULL);
}

// Worker-side control round-trip: stamp a rawnr sentinel + args onto a claimed ring and ping-pong it once.
// Used for the per-process fd-table FORK/EXIT ops (no buffer payload, no copy-back). Forwarded-syscall
// requests use the inline path in syscall_route, not this helper.
static int64_t sentry_ctl_op(uint32_t op, uint64_t a0, uint64_t a1) {
    struct sentry_ring *R = ring_for_thread();
    while (atomic_exchange_explicit(&R->busy, 1, memory_order_acquire))
        sched_yield();
    R->wpid = (uint32_t)g_worker_pid;
    R->wtid = t_token;
    R->inherit_wtid = 0;
    R->rawnr = op;
    R->a[0] = a0;
    R->a[1] = a1;
    R->iovn = 0;
    for (int i = 0; i < 6; i++)
        R->redir[i] = -1;
    uint64_t request = atomic_fetch_add_explicit(&R->request, 1, memory_order_relaxed) + 1;
    atomic_store_explicit(&R->turn, 1, memory_order_release);
    uint32_t sp = 0;
    while (atomic_load_explicit(&R->response, memory_order_acquire) != request)
        if (++sp > 256) {
            sched_yield();
            sp = 0;
        }
    int64_t result = atomic_load_explicit(&R->ret, memory_order_acquire);
    atomic_store_explicit(&R->busy, 0, memory_order_release);
    return result;
}

// SCM_RIGHTS fd passing over a control socketpair. sentry_send_fd lends one fd; sentry_recv_fd borrows it.
// A NULL control message (fd<0) is still sent so the worker's recv stays in lockstep with the round-trip.
static void sentry_send_fd(int sock, int fd) {
    struct msghdr m;
    memset(&m, 0, sizeof m);
    char b = 0;
    struct iovec io = {&b, 1};
    m.msg_iov = &io;
    m.msg_iovlen = 1;

    union {
        char buf[CMSG_SPACE(sizeof(int))];
        struct cmsghdr align;
    } u;

    if (fd >= 0) {
        memset(u.buf, 0, sizeof u.buf);
        m.msg_control = u.buf;
        m.msg_controllen = sizeof u.buf;
        struct cmsghdr *c = CMSG_FIRSTHDR(&m);
        c->cmsg_level = SOL_SOCKET;
        c->cmsg_type = SCM_RIGHTS;
        c->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(c), &fd, sizeof(int));
    }
    while (sendmsg(sock, &m, 0) < 0 && errno == EINTR) {}
}

static int sentry_recv_fd(int sock) {
    struct msghdr m;
    memset(&m, 0, sizeof m);
    char b = 0;
    struct iovec io = {&b, 1};
    m.msg_iov = &io;
    m.msg_iovlen = 1;

    union {
        char buf[CMSG_SPACE(sizeof(int))];
        struct cmsghdr align;
    } u;

    m.msg_control = u.buf;
    m.msg_controllen = sizeof u.buf;
    ssize_t r;
    while ((r = recvmsg(sock, &m, 0)) < 0 && errno == EINTR) {}
    if (r < 0) return -1;
    struct cmsghdr *c = CMSG_FIRSTHDR(&m);
    if (c && c->cmsg_level == SOL_SOCKET && c->cmsg_type == SCM_RIGHTS) {
        int fd;
        memcpy(&fd, CMSG_DATA(c), sizeof(int));
        return fd;
    }
    return -1;
}

// ioctl arg sizing (item 3). Marshaling a fixed window would clobber guest memory on copy-back (an
// FIONREAD's 4-byte int would get 256 bytes splattered over it). Modern ioctl numbers encode their size +
// direction (_IOC_SIZE/_IOC_DIR); the legacy 0x54xx terminal numbers don't, so table those. Returns the
// exact in (arg->kernel) and out (kernel->arg) byte counts for every request service_local() handles.
static void sentry_ioctl_sizes(unsigned long rq, uint32_t *insz, uint32_t *outsz) {
    uint32_t enc = (uint32_t)((rq >> 16) & 0x3fffu); // _IOC_SIZE
    uint32_t dir = (uint32_t)((rq >> 30) & 0x3u);    // 1=_IOC_WRITE(arg->kernel) 2=_IOC_READ(kernel->arg)
    if (enc) {
        *insz = (dir & 1u) ? enc : 0;
        *outsz = (dir & 2u) ? enc : 0;
        if (!dir) {
            *insz = enc;
            *outsz = enc;
        } // _IOC_NONE w/ a size: be permissive
        return;
    }
    switch (rq) {
    case 0x5401:
        *insz = 0;
        *outsz = 36;
        return;  // TCGETS    (struct termios out)
    case 0x5402: // TCSETS
    case 0x5403: // TCSETSW
    case 0x5404:
        *insz = 36;
        *outsz = 0;
        return; // TCSETSF   (struct termios in)
    case 0x5413:
        *insz = 0;
        *outsz = 8;
        return; // TIOCGWINSZ(struct winsize out)
    case 0x5414:
        *insz = 8;
        *outsz = 0;
        return; // TIOCSWINSZ(struct winsize in)
    case 0x5421:
        *insz = 4;
        *outsz = 0;
        return;  // FIONBIO   (int in)
    case 0x541b: // FIONREAD  (int out)
    case 0x540f:
        *insz = 0;
        *outsz = 4;
        return; // TIOCGPGRP (int out)
    default:
        *insz = 0;
        *outsz = 0;
        return; // FIOCLEX/FIONCLEX/TIOCSCTTY/.../unknown: no arg payload
    }
}

// ------------------------------------------------------------------ authority split
// Returns 1 if this CANONICAL syscall number carries fs/net/proc authority and must be executed by
// the sentry; 0 if it is compute/memory-only and stays LOCAL in the worker. This first PR forwards
// the read/write/open family; the comment lists the full set the production split will forward.
static int sentry_forwarded(uint64_t nr) {
    switch (nr) {
#define HL_LINUX_SENTRY_CASE(number) case number:
        HL_LINUX_SENTRY_FORWARDED(HL_LINUX_SENTRY_CASE)
#undef HL_LINUX_SENTRY_CASE
        return 1;
    // --- handled SPECIALLY in syscall_route (NOT via this table): 220/435 clone(fork) lane, 221 execve
    //     (stays local -- it reloads the guest image in-process, keeping the worker's ring/sentry), 260
    //     wait4 (reaps child WORKERS), 222 file-backed mmap (SCM_RIGHTS fd-lend). See syscall_route. ---
    default: return 0;
    }
}

// ------------------------------------------------------------------ Seatbelt (worker confinement)
// Deny-default profile for the WORKER. Anonymous memory / signals / threads are allowed; ALL file and
// network operations are denied -- the worker can reach the host ONLY through the sentry ring.
//
// SOUNDNESS: enabling this is only correct once the FULL fs/net/proc set is forwarded. With just the
// read/write/open family forwarded (this PR), any still-local fs syscall (uname/readlink/getcwd-on-
// host/...) would be denied and break a general guest. HL_SANDBOX is off by default and is
// currently sound only for guests whose entire syscall surface is the forwarded family.
static const char *k_worker_sbpl =
    "(version 1)\n"
    "(deny default)\n"
    "(allow process-fork)\n"
    "(allow process-info* (target self))\n"
    "(allow signal (target self))\n"
    "(allow sysctl-read)\n"
    // mach-lookup reaches the bootstrap server / WindowServer -- the classic macOS sandbox-escape primitive.
    // Default-deny it explicitly (belt-and-suspenders over (deny default)); the JIT worker is pure compute/
    // memory after the post-fork confinement point and needs no bootstrap services. mach-priv-task-port is
    // scoped to (target self) so a popped worker cannot grab another task's port.
    "(deny mach-lookup (global-name-regex #\".*\"))\n"
    "(allow mach-priv-task-port (target self))\n"
    "(deny file-write*)\n"    // no host writes  -- only via the sentry
    "(deny file-read-data)\n" // no host reads   -- only via the sentry
    "(deny network*)\n";      // no host sockets -- only via the sentry

#ifdef __APPLE__
extern int sandbox_init(const char *profile, uint64_t flags, char **errorbuf);
extern void sandbox_free_error(char *errorbuf);
#endif

static void worker_sandbox(void) {
#ifdef __APPLE__
    char *err = 0;
    if (sandbox_init(k_worker_sbpl, 0 /* literal profile, not SANDBOX_NAMED */, &err) != 0) {
        fprintf(stderr, "[sentry] worker Seatbelt sandbox_init failed: %s\n", err ? err : "(null)");
        if (err) sandbox_free_error(err);
        // FAIL CLOSED: an untrusted worker that could not be confined must NOT run unconfined -- abort it
        // rather than expose host fs/net directly. HL_SANDBOX is explicit opt-in.
        _exit(72);
    }
    fprintf(stderr, "[sentry] worker confined under deny-default Seatbelt profile\n");
#endif
}

// ------------------------------------------------------------------ per-process VIRTUAL fd table (P1/P2)
// SECURITY HARDENING. Each guest WORKER PROCESS gets its OWN virtual fd namespace: a sentry-PRIVATE table
// mapping the small, dense fd NUMBERS the guest sees (VIRTUAL) to the real sentry-owned fds (REAL). Two
// invariants follow:
//   (1) guest-fd virtualization -- a guest can only ever name an fd the sentry handed IT. A raw integer that
//       happens to equal a sentry-internal fd (a g_ctl[] control socket, the daemon stdio, ANOTHER guest's
//       fd) is simply not in this process's table, so it translates to -EBADF and can never address
//       sentry-internal state. Every fd the guest RECEIVES (openat/socket/accept*/dup*/pipe2/socketpair/
//       fcntl-F_DUPFD/epoll_create1/recvmsg-SCM_RIGHTS) is virtualized on the way out; every fd it PASSES is
//       translated virtual->real on the way in.
//   (2) per-process fd tables -- a forked worker gets its OWN table: fork() copies the parent's virtual->real
//       map (each inherited real fd is dup()'d so the child holds an INDEPENDENT real fd over the same open
//       file description, exactly like Linux fd inheritance), so two long-lived post-fork processes can
//       mutate/close their fds without aliasing the shared sentry fd space.
// The tables live in the sentry process's OWN memory (NOT the shared ring) so a hostile worker cannot tamper
// with them; a single global mutex serializes table mutation across the per-ring servicer threads (one sentry
// process). stdio (vfd 0/1/2) is pre-mapped 1:1 as BORROWED -- translated like any fd, but never close()'d
// when the guest drops it (that real fd is the sentry's own inherited stdio, shared with its servicer threads).
// GATE: built only under g_untrusted; the trusted fast path never touches any of this.
#define SENTRY_VFD_MAX 1024u // per-process virtual fd slots (dense; far beyond any test/jail guest's fd use)
#define SENTRY_NPROC 64u     // worker processes the sentry tracks a table for at once (the init guest + forks)
#define SENTRY_NTABLE (SENTRY_NPROC + SENTRY_NRINGS)
#define SENTRY_NBIND HL_SENTRY_BINDING_CAPACITY
#define SENTRY_NSNAP SENTRY_NPROC
#include "sentry_snapshot.h"

#include "sentry_service.c"

static void sentry_loop(void) {
    for (int i = 1; i < SENTRY_NRINGS; i++) {
        pthread_t th;
        if (pthread_create(&th, NULL, sentry_ring_thread, &g_shm->ring[i]) == 0) pthread_detach(th);
        // a failed servicer spawn just leaves ring[i] unserviced; its worker thread (if any ever claims
        // it) would block -- acceptable for the PoC pool, and never hit at <=i concurrent threads.
    }
    sentry_ring_loop(&g_shm->ring[0]); // main thread services ring 0; never returns
}

// ------------------------------------------------------------------ worker-side init / teardown
static void sentry_init(void) {
    bound_fork_state bound_fork;
    int bound_status;
    void *arena = NULL;
    if (hl_linux_shared_create(effective_host_services(), sizeof(struct sentry_shm), &arena) != HL_STATUS_OK) {
        perror("[sentry] ring mmap");
        _exit(71);
    }
    g_shm = (struct sentry_shm *)arena;
    atomic_store_explicit(&g_shm->quit, 0, memory_order_relaxed);
    atomic_store_explicit(&g_shm->claim, 0, memory_order_relaxed);
    for (int i = 0; i < SENTRY_NRINGS; i++) {
        atomic_store_explicit(&g_shm->ring[i].turn, 0, memory_order_relaxed);
        atomic_store_explicit(&g_shm->ring[i].busy, 0, memory_order_relaxed);
        atomic_store_explicit(&g_shm->ring[i].owner, 0, memory_order_relaxed);
        atomic_store_explicit(&g_shm->ring[i].request, 0, memory_order_relaxed);
        atomic_store_explicit(&g_shm->ring[i].response, 0, memory_order_relaxed);
        // Per-ring control socketpair (SCM_RIGHTS fd-lend, item 3). Created BEFORE the fork so BOTH the
        // worker and the sentry inherit both ends at the same fd numbers; each is used point-to-point by
        // the single worker thread + single sentry servicer that own that lane. A failure leaves the lane
        // without fd-lend (only file-backed mmap needs it) -- mark it -1 so we never touch a stale fd.
        if (socketpair(AF_UNIX, SOCK_DGRAM, 0, g_ctl[i]) < 0) {
            g_ctl[i][0] = -1;
            g_ctl[i][1] = -1;
        } else {
            int worker = hl_host_process_fd_private_adopt(g_ctl[i][0]);
            int service = hl_host_process_fd_private_adopt(g_ctl[i][1]);
            if (worker < 0 || service < 0) {
                if (worker >= 0) {
                    hl_host_process_fd_private_remove(worker);
                    close(worker);
                } else {
                    close(g_ctl[i][0]);
                }
                if (service >= 0) {
                    hl_host_process_fd_private_remove(service);
                    close(service);
                } else {
                    close(g_ctl[i][1]);
                }
                g_ctl[i][0] = -1;
                g_ctl[i][1] = -1;
            } else {
                g_ctl[i][0] = worker;
                g_ctl[i][1] = service;
            }
        }
    }
    g_worker_pid = getpid();
    g_sentry_owner_pid = getpid(); // only this process may signal-quit + reap the sentry
    g_guest_children = 0;
    g_worker_threads = 1;
    g_sentry_process_released = 0;
    // The authoritative ABI box and the host implementation both own process-local locks and handles.
    // Bracket the sentry helper fork exactly like a guest fork so the parent releases the speculative
    // child handles and the helper repairs its inherited host state. The helper retains its forked ABI box:
    // sentry virtual descriptors map to owned native shadows, and typed lookup happens only after that mapping,
    // so post-fork opens can keep opaque host handles without confusing virtual and logical fd numbers.
    bound_status = bound_fork_prepare(&bound_fork);
    if (bound_status != 0) {
        fprintf(stderr, "[sentry] host fork prepare failed: %d\n", bound_status);
        _exit(71);
    }
    pid_t pid = hl_host_process_clone_current(); // clone AFTER load -> inherits fd table / jail config / auxv / cwd
    int fork_error = errno;
    bound_status = bound_fork_complete(&bound_fork, pid == 0, pid == 0 ? (int)getpid() : (int)pid);
    if (bound_status != 0) {
        if (pid == 0) _exit(71);
        if (pid > 0) {
            int failed_status;
            kill(pid, SIGKILL);
            while (waitpid(pid, &failed_status, 0) < 0 && errno == EINTR) {}
        }
        fprintf(stderr, "[sentry] host fork completion failed: %d\n", bound_status);
        _exit(71);
    }
    errno = fork_error;
    if (pid < 0) {
        perror("[sentry] fork");
        _exit(71);
    }
    if (pid == 0) {
        sentry_loop(); // child: spawns the per-ring servicer threads; never returns
        _exit(0);
    }
    g_sentry_pid = pid;
    if (g_sentry_sandbox) worker_sandbox(); // confine the worker (scoped; see k_worker_sbpl note)
}

static void sentry_process_release(void) {
    // exit_group can race another guest thread reaching the final unwind boundary.  Publish ownership before
    // the synchronous control round-trip so exactly one thread releases the table and its ring; that round-trip
    // completes before its caller enters the non-returning host exit path.
    if (atomic_exchange_explicit(&g_sentry_process_released, 1, memory_order_acq_rel) != 0) return;
    sentry_ctl_op(SENTRY_OP_EXIT, 0, 0);
    ring_release();
}

// Rust-owned AArch64 exit(93) returns directly from the target syscall trap and therefore never enters
// syscall_route's exit branch.  Mirror that branch before the trap marks the CPU exited, while this worker
// can still publish the sentry lifecycle request needed to close a fork child's duplicated descriptors.
static void sentry_trapped_exit(void) {
    if (!g_untrusted) return;
    int threads = atomic_load_explicit(&g_worker_threads, memory_order_acquire);
    while (threads > 0 && !atomic_compare_exchange_weak_explicit(&g_worker_threads, &threads, threads - 1,
                                                                 memory_order_acq_rel, memory_order_acquire)) {}
    if (threads <= 0) {
        ring_release();
        return;
    }
    int process_exit = threads == 1;
    if (process_exit) {
        if (getpid() != g_sentry_owner_pid) sentry_process_release();
    } else if (t_ring >= 0) {
        sentry_ctl_op(SENTRY_OP_THREAD_EXIT, 0, 0);
    }
    ring_release();
}

static void sentry_shutdown(void) {
    if (!g_shm || !g_sentry_pid) return;
    // A forked-CHILD worker inherited g_sentry_pid but does NOT own the shared sentry: it must never set
    // quit (that would tear the sentry down under the still-live parent + sibling workers) or reap it.
    if (getpid() != g_sentry_owner_pid) {
        // Rust may intercept exit(93) before syscall_route sees it.  run_guest still unwinds here, so this
        // is the final process-lifecycle boundary that must release the child's cloned descriptor table.
        // Without it, a command-substitution child leaves its duplicated pipe writer in the sentry and the
        // parent shell blocks forever waiting for EOF before it can reap that child.
        sentry_process_release();
        g_sentry_pid = 0;
        return;
    }
    atomic_store_explicit(&g_shm->quit, 1, memory_order_release);
    int st;
    waitpid(g_sentry_pid, &st, 0);
    // This is a host lifecycle boundary, not a guest write.  In particular, fd 2 is the captured
    // guest stderr pipe here: publishing sentry statistics through it makes a silent guest appear to
    // have written diagnostics and violates the stdout/stderr contract.  Sentry teardown is therefore
    // intentionally silent; genuine guest writes still cross the ring through service_local().
    g_sentry_pid = 0; // idempotent: the exit-path shutdown + the post-run_guest teardown must not double-reap
}

// Per-process guest-state /proc files must be served by THIS worker, never the sentry. The sentry is a
// fork of the ORIGINAL worker image, so its copy of the per-process guest identity (serialized auxv,
// exe path, mapping registry, argv/environ, stack bounds) goes stale the moment any worker fork+execs a
// new image -- execve stays worker-LOCAL and the sentry never re-learns it. A guest that then reads,
// e.g., /proc/self/auxv gets the INITIAL image's AT_EXECFN/AT_PLATFORM pointers, which name memory the
// exec teardown already unmapped -> a wild dereference in the fresh image (Rust coreutils `cat` parses
// auxv at startup; HotSpot sizes its initial stack from /proc/self/stat). Match the absolute
// "/proc/self/<leaf>" and "/proc/<own container pid>/<leaf>" spellings for the leaves whose content is
// derived purely from worker-process state. The descriptor table (fd, fdinfo) IS sentry state and must
// keep forwarding.
#include "sentry_route.c"

// ------------------------------------------------------------------ NEXT sentry PR (roadmap)
// 1. Ring pool + fork/exec/wait lane -- DONE: SENTRY_NRINGS rings, RECLAIM-AWARE free-list (ring_for_thread
//    CAS-claims a free lane by per-thread token; ring_release frees it on thread/process exit), serviced by
//    N sentry THREADS in the one sentry process (shared host fd table). FORK: a guest clone/clone3 forks the
//    WORKER (the guest address space is worker-side COW the sentry cannot duplicate); the child adopts a
//    fresh identity (sentry_fork_child: drops the inherited lane, mints a new token, is NOT the sentry owner)
//    so it draws its own lane and never tears the shared sentry down. EXECVE stays local (it reloads the
//    image in-process, keeping ring/sentry/confinement). WAIT4 reaps child WORKERS, short-circuits a
//    wait-any with no guest children to -ECHILD (so it can't block on the hidden sentry child) and never
//    surfaces the sentry's pid. exit_group reap is OWNER-GATED.  PER-PROCESS VIRTUAL fd tables -- DONE: each
//    worker process gets its OWN virtual fd namespace (struct sentry_proc, keyed by the request's R->wpid).
//    Every fd the guest RECEIVES is virtualized on the way out and every fd it PASSES is translated
//    virtual->real on the way in, so a guest fd can never name a sentry-internal fd (g_ctl[]/stdio/another
//    guest's fd -> -EBADF). fork() gives the child its OWN table as a dup-COPY of the parent's map (Linux fd
//    inheritance), and a forked child releases its table (closing its owned real fds) on exit -- so two
//    long-lived post-fork processes no longer alias the shared sentry fd space.  FOLLOW-UP: the R->wpid stamp
//    is worker-supplied, so cross-PROCESS fd isolation between two cooperating malicious workers is best-effort
//    (still a strict improvement over the prior fully-shared table); bind the table to a fork-time secret if
//    that threat matters. dup3 is serviced by a direct host fcntl() (skips the dup fscache-flush nicety).
// 2. fs/net/proc forwarded set -- DONE this PR: sendmsg/recvmsg (211/212) with the full nested msghdr graph
//    (msg_name + scatter/gather msg_iov + msg_control flattened to the ring, rebased by the sentry) and
//    SCM_RIGHTS that "just works" -- a guest fd in the cmsg is ALREADY a sentry fd, so the control bytes
//    cross verbatim and a recvmsg lands a usable virtual fd. Prior PRs: socket lifecycle + two-buffer stat
//    family + iovec readv/writev + getdents64 + pread/pwrite.
// 3. Multiplexing + fd passing -- DONE this PR: ppoll/pselect6/epoll_create1/epoll_ctl/epoll_pwait forwarded
//    (they block on a sentry-owned fd, so they MUST run in the sentry; pollfd/fd_set/epoll_event marshaled
//    in+out); fd-table ops dup/dup3/pipe2/socketpair/fcntl(F_SETFL,F_DUPFD,F_GETLK flock)/ioctl(FIONBIO/
//    FIONREAD/winsize/termios, exact-size in/out) forwarded so the guest fd space stays entirely sentry-side.
//    SCM_RIGHTS worker<->sentry fd LEND: a sentry-owned fd a LOCAL worker syscall must touch (file-backed
//    mmap) is sent over a per-ring AF_UNIX control socketpair, mapped locally, then dropped.  FOLLOW-UP for
//    full soundness: forward eventfd2/timerfd/signalfd4/inotify (today they make WORKER-local fds that a
//    forwarded read/epoll_ctl then can't see); select(non-pselect)/epoll_pwait2; sendmmsg/recvmmsg (243/269).
// 4. Futex/__ulock wakeup -- STILL A SPIN (perf only, not correctness): N idle servicer threads + the worker
//    busy-wait `turn`; a process-shared futex/os_sync wake would drop idle CPU. Deferred.
// 5. Sentry-side policy: add an allow/deny layer (path allowlists, net egress) so the sentry ENFORCES
//    rather than merely executes.
