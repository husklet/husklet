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

#include "shared.h"
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
static int g_ctl[SENTRY_NRINGS][2];   // per-ring AF_UNIX control socketpair: [.][0]=worker end, [.][1]=sentry
static pid_t g_worker_pid;            // this worker process's pid (changes in a forked-child worker)
static pid_t g_sentry_owner_pid;      // ONLY the process that forked the sentry may signal-quit + reap it
static _Atomic int g_guest_children;  // live guest-forked child WORKER processes owned by THIS worker proc
static _Atomic int g_worker_threads;  // live guest threads in this worker; the initial thread contributes one
static _Atomic int g_sentry_process_released; // child process table has been released exactly once
static __thread int t_ring = -1;      // this worker thread's claimed ring index (claimed lazily on first use)
static __thread uint32_t t_token = 0; // this worker thread's unique nonzero ring-ownership token
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
    if (hl_sentry_start_take(g_thread_start, SENTRY_THREAD_STARTS, child, &token) == 0) {
        t_token = token;
    }
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

struct sentry_proc {
    uint32_t refs;                      // process roots + live thread-token bindings
    int real[SENTRY_VFD_MAX];           // virtual fd -> real sentry fd (-1 = unused slot)
    uint8_t borrowed[SENTRY_VFD_MAX];   // 1 = inherited/borrowed real fd (stdio): never close() it on drop
    uint8_t cloexec[SENTRY_VFD_MAX];    // 1 = FD_CLOEXEC set (O_CLOEXEC open / F_SETFD): swept on guest execve
    uint8_t typed[SENTRY_VFD_MAX];      // 1 = real[] names an opaque ABI descriptor shadow, not a native fd
    uint8_t procfd_dir[SENTRY_VFD_MAX]; // 1 = open description is the synthetic /proc/self/fd directory
};
static struct sentry_proc g_table[SENTRY_NTABLE];

struct sentry_process {
    pid_t wpid;
    uint16_t table;
    uint8_t inuse;
};
static struct sentry_process g_proc[SENTRY_NPROC];

static struct hl_sentry_binding g_binding[SENTRY_NBIND];

static struct hl_sentry_snapshot g_snapshot[SENTRY_NSNAP];
static struct hl_sentry_snapshots g_snapshots = {
    .slot = g_snapshot,
    .count = SENTRY_NSNAP,
};

static pthread_mutex_t g_fd_lock = PTHREAD_MUTEX_INITIALIZER; // guards process/thread tables and mappings

// Initialize a freshly claimed table: empty except stdio 0/1/2 mapped 1:1 and marked BORROWED. (All helpers
// below run with g_fd_lock held by the caller.)
static void proc_init_table(struct sentry_proc *p) {
    memset(p, 0, sizeof *p);
    p->refs = 1;
    for (uint32_t i = 0; i < SENTRY_VFD_MAX; i++) {
        p->real[i] = -1;
    }
    for (int i = 0; i < 3; i++) {
        p->real[i] = i;
        p->borrowed[i] = 1;
    }
}

static int table_claim_locked(void) {
    for (uint32_t i = 0; i < SENTRY_NTABLE; i++)
        if (g_table[i].refs == 0) {
            proc_init_table(&g_table[i]);
            return (int)i;
        }
    return -1;
}

static struct sentry_process *process_lookup_locked(pid_t wpid) {
    for (uint32_t i = 0; i < SENTRY_NPROC; i++)
        if (g_proc[i].inuse && g_proc[i].wpid == wpid) return &g_proc[i];
    return NULL;
}

static struct sentry_process *process_find_locked(pid_t wpid) {
    struct sentry_process *p = process_lookup_locked(wpid), *free_slot = NULL;
    if (p) return p;
    for (uint32_t i = 0; i < SENTRY_NPROC; i++)
        if (!g_proc[i].inuse) {
            free_slot = &g_proc[i];
            break;
        }
    if (!free_slot) return NULL;
    int table = table_claim_locked();
    if (table < 0) return NULL;
    memset(free_slot, 0, sizeof *free_slot);
    free_slot->table = (uint16_t)table;
    free_slot->wpid = wpid;
    free_slot->inuse = 1;
    return free_slot;
}

static struct hl_sentry_binding *binding_lookup_locked(pid_t wpid, uint32_t token) {
    return hl_sentry_binding_find(g_binding, SENTRY_NBIND, wpid, token);
}

static void sentry_native_close(int descriptor) {
    // Close the owned kernel object before touching descriptor-indexed emulation state.  That teardown can
    // consult/release injected logical-handle slots with the same small integer; doing it first allowed the
    // native pipe end to survive after its table entry had already been erased.
    int result = close(descriptor);
    int error = errno;
    fd_reset_emul(descriptor);
    // A sentry table owns every native descriptor passed here exactly once.  EBADF or another hard failure
    // means the ownership ledger and kernel disagree; fail closed instead of silently creating an EOF leak.
    if (result != 0 && error != EINTR) abort();
}

static void sentry_bound_close(int descriptor) {
    struct cpu tmp;
    memset(&tmp, 0, sizeof tmp);
    if (!sentry_cpu_set_canonical(&tmp, 57)) abort();
    G_A0(&tmp) = (uint64_t)(uint32_t)descriptor;
    service_local(&tmp);
}

static void sentry_owned_close(int descriptor, int typed) {
    if (typed)
        sentry_bound_close(descriptor);
    else
        sentry_native_close(descriptor);
}

static void sentry_created_close(int descriptor) {
    hl_linux_fd_snapshot snapshot;
    sentry_owned_close(descriptor, bound_snapshot((uint64_t)(uint32_t)descriptor, &snapshot));
}

static void table_release_locked(uint16_t index) {
    struct sentry_proc *table = &g_table[index];
    if (table->refs == 0 || --table->refs != 0) return;
    for (uint32_t v = 0; v < SENTRY_VFD_MAX; v++)
        if (table->real[v] >= 0 && !table->borrowed[v]) sentry_owned_close(table->real[v], table->typed[v]);
    memset(table, 0, sizeof *table);
}

static struct sentry_proc *binding_table_locked(pid_t wpid, uint32_t token, uint32_t inherit, int create) {
    struct hl_sentry_binding *binding = binding_lookup_locked(wpid, token);
    if (binding) return &g_table[binding->table];
    if (!create || token == 0) return NULL;

    struct sentry_process *process = process_lookup_locked(wpid);
    if (!process) process = process_find_locked(wpid);
    if (!process) return NULL;
    uint16_t table = process->table;
    if (inherit != 0) {
        struct hl_sentry_binding *parent = binding_lookup_locked(wpid, inherit);
        if (parent) table = parent->table;
    }
    if (hl_sentry_binding_reserve(g_binding, SENTRY_NBIND, wpid, token, table) != 0) return NULL;
    g_table[table].refs++;
    return &g_table[table];
}

static void binding_release_locked(pid_t wpid, uint32_t token) {
    struct hl_sentry_binding *binding = binding_lookup_locked(wpid, token);
    if (!binding) return;
    uint16_t table = binding->table;
    memset(binding, 0, sizeof *binding);
    table_release_locked(table);
}

static int binding_prepare_locked(pid_t wpid, uint32_t parent_token, uint32_t child_token) {
    if (child_token == 0 || binding_lookup_locked(wpid, child_token)) return -EINVAL;
    struct sentry_proc *source = binding_table_locked(wpid, parent_token, 0, 1);
    if (!source) return -ENOMEM;
    uint16_t table = (uint16_t)(source - g_table);
    int result = hl_sentry_binding_reserve(g_binding, SENTRY_NBIND, wpid, child_token, table);
    if (result == -EEXIST) return -EINVAL;
    if (result == 0) g_table[table].refs++;
    return result;
}

// Allocate the lowest free virtual fd >= minv, map it to (owned, closeable) real fd `rfd`. Returns vfd, or -1
// if the table is full (caller closes `rfd` and returns -EMFILE -- never leaks the real fd to the guest).
static int vfd_alloc(struct sentry_proc *p, int rfd, uint32_t minv) {
    hl_linux_fd_snapshot snapshot;
    for (uint32_t v = minv; v < SENTRY_VFD_MAX; v++)
        if (p->real[v] < 0) {
            p->real[v] = rfd;
            p->borrowed[v] = 0;
            p->typed[v] = bound_snapshot((uint64_t)(uint32_t)rfd, &snapshot) != 0;
            return (int)v;
        }
    return -1;
}

// Translate a guest virtual fd to its real sentry fd, or -1 if it is not mapped in this table (=> -EBADF).
static int vfd_real(struct sentry_proc *p, int vfd) {
    if (vfd < 0 || (uint32_t)vfd >= SENTRY_VFD_MAX) return -1;
    return p->real[vfd];
}

// Translate an exact procfs descriptor link from the guest's virtual descriptor namespace into the
// sentry's real descriptor namespace.  The path is consumed later by service_local(), whose procfs
// implementation necessarily sees sentry-owned descriptors; forwarding the guest number unchanged can
// therefore alias an unrelated internal descriptor.  Return 1 when translated, 0 for a non-procfd path,
// and -1 when the path names an unmapped guest descriptor.
static int vfd_proc_path(struct sentry_proc *p, char *path, size_t cap) {
    static const char dev_prefix[] = "/dev/fd/";
    static const char self_prefix[] = "/proc/self/fd/";
    const char *digits = NULL;
    size_t prefix_len = 0;
    if (strncmp(path, dev_prefix, sizeof dev_prefix - 1) == 0) {
        digits = path + sizeof dev_prefix - 1;
        prefix_len = sizeof dev_prefix - 1;
    } else if (strncmp(path, self_prefix, sizeof self_prefix - 1) == 0) {
        digits = path + sizeof self_prefix - 1;
        prefix_len = sizeof self_prefix - 1;
    } else {
        return 0;
    }
    if (*digits < '0' || *digits > '9') return 0;
    uint32_t vfd = 0;
    for (const char *s = digits; *s; s++) {
        if (*s < '0' || *s > '9') return 0; // only an exact descriptor-link leaf is translated
        if (vfd >= SENTRY_VFD_MAX || vfd > (UINT32_MAX - (uint32_t)(*s - '0')) / 10u) return -1;
        vfd = vfd * 10u + (uint32_t)(*s - '0');
    }
    int real = vfd_real(p, (int)vfd);
    if (real < 0) return -1;
    int n = snprintf(path + prefix_len, cap - prefix_len, "%d", real);
    return n >= 0 && (size_t)n < cap - prefix_len ? (p->typed[vfd] ? 1 : 2) : -1;
}

// Drop a guest virtual fd from the table. Returns the real fd the caller must close(), or -1 if the entry was
// BORROWED (stdio) or unmapped -- in which case the caller must NOT close the real fd.
static int vfd_drop(struct sentry_proc *p, int vfd) {
    if (vfd < 0 || (uint32_t)vfd >= SENTRY_VFD_MAX || p->real[vfd] < 0) return -1;
    int rfd = p->borrowed[vfd] ? -1 : p->real[vfd];
    p->real[vfd] = -1;
    p->borrowed[vfd] = 0;
    p->cloexec[vfd] = 0;
    p->typed[vfd] = 0;
    p->procfd_dir[vfd] = 0;
    return rfd;
}

static int table_clone_locked(const struct sentry_proc *source) {
    int index = table_claim_locked();
    if (index < 0) return -1;
    struct sentry_proc *copy = &g_table[index];
    for (uint32_t v = 0; v < SENTRY_VFD_MAX; v++) {
        if (source->real[v] < 0) continue;
        copy->cloexec[v] = source->cloexec[v];
        copy->typed[v] = source->typed[v];
        copy->procfd_dir[v] = source->procfd_dir[v];
        if (source->borrowed[v]) {
            copy->real[v] = source->real[v];
            copy->borrowed[v] = 1;
            continue;
        }
        int duplicate;
        if (source->typed[v]) {
            hl_linux_fd_snapshot typed;
            if (!bound_snapshot((uint64_t)(uint32_t)source->real[v], &typed)) {
                table_release_locked((uint16_t)index);
                return -1;
            }
            duplicate = (int)bound_dup_at_least(typed.fd, 0, source->cloexec[v] ? HL_LINUX_FD_CLOEXEC : 0);
        } else {
            duplicate = dup(source->real[v]);
        }
        if (duplicate < 0) {
            table_release_locked((uint16_t)index);
            return -1;
        }
        copy->real[v] = duplicate;
        hl_native_kqueue_duplicate(source->real[v], duplicate);
        if (duplicate < HL_NFD && source->real[v] >= 0 && source->real[v] < HL_NFD) {
            strcpy(g_fdpath[duplicate], g_fdpath[source->real[v]]);
            strcpy(g_proc_text_desc[duplicate], g_proc_text_desc[source->real[v]]);
            g_proc_text_ro[duplicate] = g_proc_text_ro[source->real[v]];
            g_pagemap_fd[duplicate] = g_pagemap_fd[source->real[v]];
            fd_carry_sock(duplicate, source->real[v]);
        }
    }
    return index;
}

static struct sentry_proc *table_unshare_locked(pid_t wpid, uint32_t token, uint32_t inherit) {
    if (token == 0) return NULL;
    struct sentry_proc *current = binding_table_locked(wpid, token, inherit, 1);
    if (!current) return NULL;
    struct hl_sentry_binding *binding = binding_lookup_locked(wpid, token);
    if (!binding) return NULL;
    int clone = table_clone_locked(current);
    if (clone < 0) return NULL;
    uint16_t previous = binding->table;
    binding->table = (uint16_t)clone;
    table_release_locked(previous);
    return &g_table[clone];
}

static int64_t sentry_fork_prepare(pid_t parent, uint32_t token, uint32_t inherit) {
    pthread_mutex_lock(&g_fd_lock);
    struct sentry_proc *source = binding_table_locked(parent, token, inherit, 1);
    int table = source ? table_clone_locked(source) : -1;
    if (table < 0) {
        pthread_mutex_unlock(&g_fd_lock);
        return -ENOMEM;
    }
    int64_t handle = hl_sentry_snapshot_reserve(&g_snapshots, parent, token, (uint16_t)table);
    if (handle < 0) {
        table_release_locked((uint16_t)table);
        pthread_mutex_unlock(&g_fd_lock);
        return handle;
    }
    pthread_mutex_unlock(&g_fd_lock);
    return handle;
}

static int sentry_fork_cancel(pid_t owner, uint32_t token, uint64_t handle) {
    pthread_mutex_lock(&g_fd_lock);
    uint16_t table = 0;
    int result = hl_sentry_snapshot_take(&g_snapshots, owner, token, handle, &table);
    if (result == 0) table_release_locked(table);
    pthread_mutex_unlock(&g_fd_lock);
    return result;
}

// Bind the immutable pre-fork snapshot to the child identity before the private fork barrier releases it.
// The child's first token binding then inherits this process root.
static int sentry_proc_fork(pid_t owner, uint32_t token, uint64_t handle, pid_t child) {
    pthread_mutex_lock(&g_fd_lock);
    if (process_lookup_locked(child)) {
        pthread_mutex_unlock(&g_fd_lock);
        return -EEXIST;
    }
    struct hl_sentry_snapshot *snapshot = hl_sentry_snapshot_find(&g_snapshots, owner, token, handle);
    if (!snapshot) {
        pthread_mutex_unlock(&g_fd_lock);
        return -EINVAL;
    }
    struct sentry_process *process = NULL;
    for (uint32_t i = 0; i < SENTRY_NPROC; i++)
        if (!g_proc[i].inuse) {
            process = &g_proc[i];
            *process = (struct sentry_process){
                .wpid = child,
                .table = snapshot->payload,
                .inuse = 1,
            };
            break;
        }
    if (!process) {
        pthread_mutex_unlock(&g_fd_lock);
        return -EAGAIN;
    }
    uint16_t table = 0;
    if (hl_sentry_snapshot_take(&g_snapshots, owner, token, handle, &table) != 0 || table != process->table) abort();
    pthread_mutex_unlock(&g_fd_lock);
    return 0;
}

// Release a worker process's table on its exit: close every OWNED real fd it still holds and free the slot.
// (Borrowed stdio is never closed -- it belongs to the sentry.) The init guest's table is reclaimed by the
// sentry process tearing down; only forked children call this.
static void sentry_proc_release(pid_t wpid) {
    pthread_mutex_lock(&g_fd_lock);
    uint16_t snapshot_table = 0;
    while (hl_sentry_snapshot_take_owner(&g_snapshots, wpid, &snapshot_table))
        table_release_locked(snapshot_table);
    struct sentry_process *p = process_lookup_locked(wpid);
    if (p) {
        uint16_t table = p->table;
        memset(p, 0, sizeof *p);
        table_release_locked(table);
    }
    for (uint32_t i = 0; i < SENTRY_NBIND; i++)
        if (g_binding[i].inuse && g_binding[i].owner == wpid) {
            uint16_t table = g_binding[i].table;
            memset(&g_binding[i], 0, sizeof g_binding[i]);
            table_release_locked(table);
        }
    pthread_mutex_unlock(&g_fd_lock);
}

// guest execve close-on-exec sweep: a guest execve stays local (service_local reloads the image in this
// worker), so nothing closes the FD_CLOEXEC-marked virtual fds the way a real execve would. Walk the
// worker's table and close+drop every OWNED cloexec fd (stdio/borrowed is never closed). Fds WITHOUT
// FD_CLOEXEC survive, exactly as Linux keeps them open across execve.
static int sentry_proc_exec_sweep(pid_t wpid, uint32_t token) {
    pthread_mutex_lock(&g_fd_lock);
    if (!binding_table_locked(wpid, token, 0, 1)) {
        pthread_mutex_unlock(&g_fd_lock);
        return -EAGAIN;
    }
    struct hl_sentry_binding *caller = binding_lookup_locked(wpid, token);
    struct sentry_process *process = process_lookup_locked(wpid);
    struct sentry_proc *p = caller ? &g_table[caller->table] : NULL;
    if (caller && process && process->table != caller->table) {
        uint16_t previous = process->table;
        process->table = caller->table;
        p->refs++;
        table_release_locked(previous);
    }
    for (uint32_t i = 0; i < SENTRY_NBIND; i++)
        if (g_binding[i].inuse && g_binding[i].owner == wpid && g_binding[i].token != token) {
            uint16_t table = g_binding[i].table;
            memset(&g_binding[i], 0, sizeof g_binding[i]);
            table_release_locked(table);
        }
    if (p)
        for (uint32_t v = 0; v < SENTRY_VFD_MAX; v++)
            if (p->real[v] >= 0 && p->cloexec[v]) {
                int typed = p->typed[v];
                int rfd = vfd_drop(p, (int)v);
                if (rfd >= 0) sentry_owned_close(rfd, typed);
            }
    pthread_mutex_unlock(&g_fd_lock);
    return 0;
}

// SENDMSG SCM_RIGHTS (P2 finding G, virtualized): translate every guest VFD in a (Linux-layout, PRIVATE) cmsg
// buffer to its real sentry fd IN PLACE. Returns 0 if every passed fd was a live guest fd (a correct guest
// only ever passes its own, so this is always 0 for it), -1 if any was not mapped -- in which case the whole
// sendmsg is rejected, so a smuggled sentry-internal fd (g_ctl[]/ring/daemon) can never reach the wire.
// Caller holds g_fd_lock. Strictly bounded by `len` -- never derefs past it.
static int sentry_cmsg_translate_out(struct sentry_proc *p, uint8_t *ctl, size_t len) {
    size_t o = 0;
    while (o + 16u <= len) { // Linux struct cmsghdr: {u64 cmsg_len; int level; int type}
        uint64_t clen = *(const uint64_t *)(ctl + o);
        int level = *(const int *)(ctl + o + 8);
        int type = *(const int *)(ctl + o + 12);
        if (clen < 16u || o + clen > len) break;
        if (level == LX_SOL_SOCKET && type == SCM_RIGHTS) {
            size_t nfd = (size_t)(clen - 16u) / sizeof(int);
            for (size_t i = 0; i < nfd; i++) {
                int *slot = (int *)(ctl + o + 16u + i * sizeof(int));
                int rfd = hl_sentry_native_fd(p->real, p->typed, SENTRY_VFD_MAX, *slot);
                if (rfd < 0) return -1; // not a native fd owned by this guest -> reject the whole sendmsg
                *slot = rfd;
            }
        }
        o += (size_t)((clen + 7u) & ~(uint64_t)7u); // CMSG_ALIGN to 8
    }
    return 0;
}

// RECVMSG SCM_RIGHTS (virtualized): the sentry received real fds; allocate a guest VFD for each and rewrite it
// IN PLACE so the guest only ever sees virtual fds. An exhausted table closes the real fd and writes -1.
// Caller holds g_fd_lock. Strictly bounded by `len` -- never derefs past it.
static void sentry_cmsg_translate_in(struct sentry_proc *p, uint8_t *ctl, size_t len) {
    size_t o = 0;
    while (o + 16u <= len) {
        uint64_t clen = *(uint64_t *)(ctl + o);
        int level = *(int *)(ctl + o + 8);
        int type = *(int *)(ctl + o + 12);
        if (clen < 16u || o + clen > len) break;
        if (level == LX_SOL_SOCKET && type == SCM_RIGHTS) {
            size_t nfd = (size_t)(clen - 16u) / sizeof(int);
            for (size_t i = 0; i < nfd; i++) {
                int *slot = (int *)(ctl + o + 16u + i * sizeof(int));
                int v = vfd_alloc(p, *slot, 0);
                if (v < 0) {
                    sentry_native_close(*slot);
                    *slot = -1;
                } else {
                    *slot = v;
                }
            }
        }
        o += (size_t)((clen + 7u) & ~(uint64_t)7u);
    }
}

// 1 if this canonical syscall carries its OPERATING fd in the a0 register (so the boundary translates a0
// virtual->real). The fd-bearing-but-NOT-a0 cases (openat/stat dirfd, dup3 newfd, epoll_ctl target, ppoll/
// pselect fd containers) are handled explicitly in sentry_service_one.
static int fd_in_a0(uint64_t nr) {
    switch (nr) {
    case 46:
    case 47: // ftruncate/fallocate
    case 61:
    case 62:
    case 63:
    case 64:
    case 65:
    case 66:
    case 67:
    case 68:
    case 71:
    case 80: // fs r/w/seek/stat
    case 200:
    case 201:
    case 202:
    case 203:
    case 204:
    case 205:
    case 206:
    case 207: // socket family
    case 208:
    case 209:
    case 210:
    case 211:
    case 212:
    case 242: // sockopt/shutdown/msg/accept4
    case 23:
    case 25:
    case 29:
    case 22: // dup/fcntl/ioctl/epoll_pwait
        return 1;
    default: return 0;
    }
}

// ------------------------------------------------------------------ sentry process body
// Holds host authority. Services ONE marshaled request on ring R: rebuilds a cpu from the marshaled
// registers, redirects each flagged guest-buffer pointer arg into the shared ring (so service_local()
// never touches worker/guest memory) -- including rebasing the flattened readv/writev iovec offsets to
// ring pointers -- and runs the REAL service_local() -- identical jail/proc/overlay policy, identical
// bytes. NOTE: it MUST call service_local() (the canonical switch), not service() -- service() would
// re-enter syscall_route() in this (g_untrusted) process and recurse onto the ring.
static void sentry_service_one(struct sentry_ring *R) {
    // fd-lend (item 3): not a syscall -- lend a sentry-owned fd to the worker over THIS ring's control
    // socketpair (SCM_RIGHTS) for a file-backed mmap; the worker maps it locally then drops it. OWNERSHIP
    // (P1, finding F): the lendable fd MUST be one the sentry opened ON BEHALF OF THE GUEST (tracked at
    // openat/socket/accept/dup/pipe2/socketpair/...). An arbitrary worker-named integer -- the sentry's own
    // g_ctl[] control socket, the daemon stdio, any non-guest host fd -- is rejected -EBADF. Detected before
    // any cpu reconstruction. We ALWAYS send a control datagram (with the fd, or empty on reject) so the
    // worker's matching recv stays in lockstep with the round-trip and never desyncs the next lend.
    if (R->rawnr == SENTRY_OP_FDPASS) {
        int idx = (int)(R - g_shm->ring);
        pthread_mutex_lock(&g_fd_lock);
        struct sentry_proc *p = binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
        int vfd = (int)(int64_t)R->a[0];
        int rfd = p ? hl_sentry_native_fd(p->real, p->typed, SENTRY_VFD_MAX, vfd) : -1;
        pthread_mutex_unlock(&g_fd_lock);
        if (rfd >= 0) {
            sentry_send_fd(g_ctl[idx][1], rfd);
            R->ret = 0;
        } else {
            sentry_send_fd(g_ctl[idx][1], -1); // empty datagram: keep the worker recv in lockstep
            R->ret = -EBADF;
        }
        R->nserved++;
        return;
    }
    // Reverse adoption (SENTRY_OP_ADOPT): receive a worker-opened real fd from the ring's control
    // socketpair and install it into the calling worker's virtual fd table. The datagram was queued by
    // the worker BEFORE it handed the turn over, so this recv never blocks on a missing message.
    if (R->rawnr == SENTRY_OP_ADOPT) {
        int idx = (int)(R - g_shm->ring);
        int rfd = (idx >= 0 && g_ctl[idx][1] >= 0) ? sentry_recv_fd(g_ctl[idx][1]) : -1;
        if (rfd < 0) {
            R->ret = -EIO;
            R->nserved++;
            return;
        }
        pthread_mutex_lock(&g_fd_lock);
        struct sentry_proc *p = binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
        int v = p ? vfd_alloc(p, rfd, 0) : -1;
        if (v >= 0) p->cloexec[v] = (uint8_t)(R->a[0] != 0);
        pthread_mutex_unlock(&g_fd_lock);
        if (v < 0) {
            sentry_native_close(rfd);
            R->ret = -EMFILE;
        } else {
            R->ret = v;
        }
        R->nserved++;
        return;
    }
    // Per-process fd-table control ops (P1/P2): clone the parent's map into a fresh child table on fork;
    // release a worker's table (close its owned real fds) on exit. Neither reconstructs a cpu.
    if (R->rawnr == SENTRY_OP_FORK_PREPARE) {
        R->ret = sentry_fork_prepare((pid_t)R->wpid, R->wtid, R->inherit_wtid);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_FORK) {
        R->ret = sentry_proc_fork((pid_t)R->wpid, R->wtid, R->a[0], (pid_t)R->a[1]);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_FORK_CANCEL) {
        R->ret = sentry_fork_cancel((pid_t)R->wpid, R->wtid, R->a[0]);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_EXEC) {
        R->ret = sentry_proc_exec_sweep((pid_t)R->wpid, R->wtid);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_EXIT) {
        sentry_proc_release((pid_t)R->wpid);
        R->ret = 0;
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_REAP) {
        sentry_proc_release((pid_t)R->a[0]);
        R->ret = 0;
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_THREAD_PREPARE) {
        pthread_mutex_lock(&g_fd_lock);
        R->ret = binding_prepare_locked((pid_t)R->wpid, R->wtid, (uint32_t)R->a[0]);
        pthread_mutex_unlock(&g_fd_lock);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_BIND) {
        pthread_mutex_lock(&g_fd_lock);
        R->ret = binding_table_locked((pid_t)R->wpid, R->wtid, 0, 1) ? 0 : -EAGAIN;
        pthread_mutex_unlock(&g_fd_lock);
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_THREAD_CANCEL) {
        pthread_mutex_lock(&g_fd_lock);
        binding_release_locked((pid_t)R->wpid, (uint32_t)R->a[0]);
        pthread_mutex_unlock(&g_fd_lock);
        R->ret = 0;
        R->nserved++;
        return;
    }
    if (R->rawnr == SENTRY_OP_THREAD_EXIT) {
        pthread_mutex_lock(&g_fd_lock);
        binding_release_locked((pid_t)R->wpid, R->wtid);
        pthread_mutex_unlock(&g_fd_lock);
        R->ret = 0;
        R->nserved++;
        return;
    }
    if (R->rawnr == 436) { /* close_range over this process's virtual descriptor table */
        uint32_t first = (uint32_t)R->a[0], last = (uint32_t)R->a[1];
        uint32_t flags = (uint32_t)R->a[2];
        if ((flags & ~(uint32_t)(2u | 4u)) != 0 || first > last) {
            R->ret = -EINVAL;
        } else {
            if (last >= SENTRY_VFD_MAX) last = SENTRY_VFD_MAX - 1;
            pthread_mutex_lock(&g_fd_lock);
            struct sentry_proc *p = (flags & 2u) ? table_unshare_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid)
                                                 : binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
            if ((flags & 2u) && p == NULL) {
                pthread_mutex_unlock(&g_fd_lock);
                R->ret = -ENOMEM;
                R->nserved++;
                return;
            }
            if (p != NULL && first < SENTRY_VFD_MAX)
                for (uint32_t v = first; v <= last; ++v) {
                    if (p->real[v] < 0) continue;
                    if ((flags & 4u) != 0) {
                        p->cloexec[v] = 1;
                    } else {
                        int typed = p->typed[v];
                        int real = vfd_drop(p, (int)v);
                        if (real >= 0) sentry_owned_close(real, typed);
                    }
                }
            pthread_mutex_unlock(&g_fd_lock);
            R->ret = 0;
        }
        R->nserved++;
        return;
    }
    struct cpu tmp;
    memset(&tmp, 0, sizeof tmp);
    G_RAWNR(&tmp) = R->rawnr; // service_local() re-runs G_NORMALIZE on this as a no-op (already *at)
    G_A0(&tmp) = R->a[0];
    G_A1(&tmp) = R->a[1];
    G_A2(&tmp) = R->a[2];
    G_A3(&tmp) = R->a[3];
    G_A4(&tmp) = R->a[4];
    G_A5(&tmp) = R->a[5];
    // Snapshot the pointer-redirect metadata into sentry-PRIVATE locals: from here on every validation reads
    // these copies, NEVER the attacker-writable shared ring, so a racing worker thread cannot rewrite a
    // field between our check and the kernel's use of it (the validate-in-place TOCTOU, finding E). Scalars
    // are already snapshotted into `tmp`; this extends the same discipline to the redir table + iovn.
    int32_t redir[6];
    for (int i = 0; i < 6; i++)
        redir[i] = R->redir[i];
    uint32_t iovn = R->iovn;

    // Redirect each flagged pointer arg into the ring buffer (THE crossing point: from here on
    // service_local() touches only ring/private memory). Bounds-check every worker-supplied offset first --
    // an out-of-range offset is a hijacked/buggy worker and faults the call rather than the sentry.
    int bad = 0;
    uint32_t off[6] = {0, 0, 0, 0, 0, 0};
    int have[6] = {0, 0, 0, 0, 0, 0};
    uint64_t *ta[6] = {&G_A0(&tmp), &G_A1(&tmp), &G_A2(&tmp), &G_A3(&tmp), &G_A4(&tmp), &G_A5(&tmp)};
    for (int i = 0; i < 6; i++) {
        if (redir[i] < 0) continue;
        uint32_t o = (uint32_t)redir[i];
        if (o >= SENTRY_BUFSZ) {
            bad = 1;
            break;
        }
        off[i] = o;
        have[i] = 1;
        *ta[i] = (uint64_t)(R->buf + o);
    }

    uint64_t snr = bad ? 0 : G_NR(&tmp);
    if (snr == 220 || snr == 435) {
        // Process creation is worker-local memory authority. A stale or corrupted mailbox request must
        // never make a sentry servicer fork into a second consumer of the shared rings.
        R->ret = -EPERM;
        R->nserved++;
        return;
    }

    // Per-servicer-thread PRIVATE iovec[] -- the kernel scatters/gathers through THIS, not the shared ring,
    // so a racing worker thread cannot move a segment after we validated it (finding E). 16B/seg * IOVMAX.
    static __thread struct iovec piov[SENTRY_IOVMAX];
    socklen_t pslen = 0;            // PRIVATE in/out socklen: the kernel never sources the length from shared memory
    int slen_back = 0;              // after the call, mirror pslen back into the SLEN window for the worker copy-back
    uint8_t ph[64];                 // PRIVATE Linux-layout 56-byte msghdr copy (sendmsg/recvmsg graph)
    uint8_t pctl[SENTRY_MSGCTLCAP]; // PRIVATE sendmsg cmsg copy (validated SCM_RIGHTS fds, race-free; finding G)
    int msg_built = 0;
    uint64_t coff = 0; // recvmsg control-window offset (for the SCM_RIGHTS fd-track after the call)

    // ---- P0 finding A/D: clamp EVERY length the kernel will use to read/write buf[] down to the bytes
    //      actually remaining in that ring window (BUFSZ - offset). Correct traffic is already inside its
    //      window, so the min() is a no-op for it; only a hostile over-large length is cut. The worker-side
    //      caps are NOT a security control -- this is the sentry re-deriving the bound from the redir window.
    //      In/out socklen/optlen values are routed through PRIVATE storage (pslen) so the kernel reads the
    //      clamped capacity from sentry memory, race-free, and the output is mirrored back afterwards. ----
    if (!bad) {
        switch (snr) {
        case 61:
        case 63:
        case 67: // getdents64 / read / pread64: a2 = byte count through buf+off[1]
        case 64:
        case 68: // write / pwrite64
        case 200:
        case 203: // bind / connect: a2 = addrlen through buf+off[1]
            if (have[1] && G_A2(&tmp) > (uint64_t)(SENTRY_BUFSZ - off[1])) G_A2(&tmp) = SENTRY_BUFSZ - off[1];
            break;
        case 206: // sendto: a2 = data len (off[1]); a5 = destaddr len (off[4])
            if (have[1] && G_A2(&tmp) > (uint64_t)(SENTRY_BUFSZ - off[1])) G_A2(&tmp) = SENTRY_BUFSZ - off[1];
            if (have[4] && G_A5(&tmp) > (uint64_t)(SENTRY_BUFSZ - off[4])) G_A5(&tmp) = SENTRY_BUFSZ - off[4];
            break;
        case 207: // recvfrom: a2 = data len; a5 = in/out socklen -> PRIVATE (clamped to window)
            if (have[1] && G_A2(&tmp) > (uint64_t)(SENTRY_BUFSZ - off[1])) G_A2(&tmp) = SENTRY_BUFSZ - off[1];
            if (have[5]) {
                pslen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF);
                if (pslen > SENTRY_SADDRCAP) pslen = SENTRY_SADDRCAP;
                G_A5(&tmp) = (uint64_t)&pslen;
                slen_back = 1;
            }
            break;
        case 202:
        case 242: // accept / accept4
        case 204:
        case 205: // getsockname / getpeername: a2 = in/out socklen -> PRIVATE (clamped)
            if (have[2]) {
                pslen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF);
                if (pslen > SENTRY_SADDRCAP) pslen = SENTRY_SADDRCAP;
                G_A2(&tmp) = (uint64_t)&pslen;
                slen_back = 1;
            }
            break;
        case 208: // setsockopt: a4 = optlen through buf+off[3]
            if (have[3] && G_A4(&tmp) > (uint64_t)(SENTRY_BUFSZ - off[3])) G_A4(&tmp) = SENTRY_BUFSZ - off[3];
            break;
        case 209: // getsockopt: a4 = in/out optlen -> PRIVATE (clamped to the optval window)
            if (have[4]) {
                pslen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF);
                if (pslen > SENTRY_OPTCAP) pslen = SENTRY_OPTCAP;
                G_A4(&tmp) = (uint64_t)&pslen;
                slen_back = 1;
            }
            break;
        case 73: // ppoll: a1 = nfds (8B/entry) into the pollfd window [0,DATACAP)
            if (G_A1(&tmp) > (uint64_t)(SENTRY_DATACAP / 8u)) G_A1(&tmp) = SENTRY_DATACAP / 8u;
            break;
        case 72: // pselect6: a0 = nfds -> (nfds+7)/8 <= 128B fits each fd_set window
            if (G_A0(&tmp) > 1024u) G_A0(&tmp) = 1024u;
            break;
        case 22: // epoll_pwait: a2 = maxevents (SENTRY_EPEV_SZ/entry) into the out window [0,BUFSZ)
            if (have[1] && G_A2(&tmp) > (uint64_t)(SENTRY_BUFSZ / SENTRY_EPEV_SZ))
                G_A2(&tmp) = SENTRY_BUFSZ / SENTRY_EPEV_SZ;
            break;
        case 48:
        case 56:
        case 78:
        case 79:
        case 291: // openat / newfstatat / statx: force the in-path NUL-terminated within
        case 439:
            R->buf[SENTRY_PATHCAP - 1] = 0; // its window so service_local()'s C-string walk can't run off buf
            break;
        default: break;
        }
    }

    // ---- P0 finding B/E: readv/writev -- bound the segment count, reject a wild base, then COPY the iovec[]
    //      descriptor array OUT of the shared ring into private memory, validate the copy, and point the
    //      kernel at it. (We also mirror the validated descriptors back into buf[] for the worker's own
    //      scatter copy-back; that read is worker-side / intra-principal, not a sentry crossing.) ----
    if (!bad && iovn) {
        if (!have[1]) {
            bad = 1; // iovn>0 with no valid a1 redir window would be a wild deref off buf[] -- reject (finding B.2)
        } else {
            uint32_t maxn = (uint32_t)((SENTRY_BUFSZ - off[1]) / sizeof(struct iovec));
            if (iovn > SENTRY_IOVMAX) iovn = SENTRY_IOVMAX;
            if (iovn > maxn) iovn = maxn;
            struct iovec *iv = (struct iovec *)(R->buf + off[1]); // shared (attacker-writable)
            for (uint32_t k = 0; k < iovn; k++) {
                uint64_t boff = (uint64_t)(uintptr_t)iv[k].iov_base, len = iv[k].iov_len; // read ONCE
                if (boff > SENTRY_BUFSZ || len > SENTRY_BUFSZ || boff + len > SENTRY_BUFSZ) {
                    piov[k].iov_base = R->buf;
                    piov[k].iov_len = 0; // bad seg -> empty (don't escape the ring)
                    iv[k].iov_base = R->buf;
                    iv[k].iov_len = 0;
                } else {
                    piov[k].iov_base = R->buf + boff;
                    piov[k].iov_len = (size_t)len;
                    iv[k].iov_base = R->buf + boff;
                    iv[k].iov_len = (size_t)len;
                }
            }
            G_A1(&tmp) = (uint64_t)piov; // kernel reads the PRIVATE iovec[]
            G_A2(&tmp) = iovn;
        }
    }

    // ---- P0 finding C/E: sendmsg/recvmsg -- build the WHOLE msghdr graph in private memory: a Linux-layout
    //      56-byte header pointing at the private iovec[], with msg_namelen/msg_controllen clamped to their
    //      windows. service_local() reads/writes this private header; nothing it touches is re-read by the
    //      kernel from attacker-writable shared memory. (R->iovn stays 0 for these so the block above is skipped.)
    if (!bad && (snr == 211 || snr == 212)) {
        uint8_t *h = R->buf;
        uint64_t noff = *(uint64_t *)(h + 0);
        uint32_t nlen = *(uint32_t *)(h + 8);
        uint64_t ioff = *(uint64_t *)(h + 16);
        uint64_t in = *(uint64_t *)(h + 24);
        uint64_t clen = *(uint64_t *)(h + 40);
        uint32_t mflags = *(uint32_t *)(h + 48);
        coff = *(uint64_t *)(h + 32);
        if (noff >= SENTRY_BUFSZ || ioff >= SENTRY_BUFSZ || coff >= SENTRY_BUFSZ) {
            bad = 1;
        } else {
            memset(ph, 0, sizeof ph);
            if (noff) {
                if (nlen > (uint32_t)(SENTRY_BUFSZ - noff)) nlen = (uint32_t)(SENTRY_BUFSZ - noff);
                *(uint64_t *)(ph + 0) = (uint64_t)(R->buf + noff); // msg_name -> ring ptr
                *(uint32_t *)(ph + 8) = nlen;                      // msg_namelen, clamped to window
            }
            uint32_t n = 0;
            if (ioff) {
                uint32_t maxn = (uint32_t)((SENTRY_BUFSZ - ioff) / sizeof(struct iovec));
                n = (in > SENTRY_IOVMAX) ? SENTRY_IOVMAX : (uint32_t)in; // bound msg_iovlen (finding C)
                if (n > maxn) n = maxn;
                struct iovec *iv = (struct iovec *)(R->buf + ioff);
                for (uint32_t k = 0; k < n; k++) {
                    uint64_t boff = (uint64_t)(uintptr_t)iv[k].iov_base, len = iv[k].iov_len;
                    if (boff > SENTRY_BUFSZ || len > SENTRY_BUFSZ || boff + len > SENTRY_BUFSZ) {
                        piov[k].iov_base = R->buf;
                        piov[k].iov_len = 0;
                        iv[k].iov_base = R->buf;
                        iv[k].iov_len = 0;
                    } else {
                        piov[k].iov_base = R->buf + boff;
                        piov[k].iov_len = (size_t)len;
                        iv[k].iov_base = R->buf + boff;
                        iv[k].iov_len = (size_t)len;
                    }
                }
                *(uint64_t *)(ph + 16) = (uint64_t)piov; // msg_iov -> PRIVATE iovec[]
            }
            *(uint64_t *)(ph + 24) = n;
            if (coff) {
                if (clen > (uint64_t)(SENTRY_BUFSZ - coff)) clen = SENTRY_BUFSZ - coff;
                if (snr == 211) {
                    // ---- P2 finding G: OUTBOUND SCM_RIGHTS fd validation. A guest sendmsg may only emit fds
                    //      the sentry handed it. Copy the cmsg into PRIVATE memory FIRST (so the validation is
                    //      race-free vs a concurrent worker thread rewriting the ring -- finding E), then verify
                    //      every SCM_RIGHTS fd is guest-owned. If any is not (a smuggled g_ctl[]/ring/daemon fd),
                    //      fail the WHOLE call -EPERM -- simplest and clearly correct; a correct guest only ever
                    //      passes its own fds so all pass and this never fires for it. service_local then sends
                    //      from the validated PRIVATE copy, not attacker-writable shared memory. ----
                    uint64_t ccap = clen > SENTRY_MSGCTLCAP ? SENTRY_MSGCTLCAP : clen; // legit cmsg already <= cap
                    memcpy(pctl, R->buf + coff, (size_t)ccap);
                    // VIRTUALIZE the SCM_RIGHTS fds in the PRIVATE copy: translate each guest VFD -> its real
                    // sentry fd. A non-guest fd (smuggled g_ctl[]/ring/daemon fd) is not in the table -> reject
                    // the whole sendmsg -EPERM, so it can never reach the wire.
                    pthread_mutex_lock(&g_fd_lock);
                    struct sentry_proc *cp = binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
                    int ctl_ok = cp && sentry_cmsg_translate_out(cp, pctl, (size_t)ccap) == 0;
                    pthread_mutex_unlock(&g_fd_lock);
                    if (!ctl_ok) {
                        R->ret = -EPERM;
                        R->nserved++;
                        return;
                    }
                    *(uint64_t *)(ph + 32) = (uint64_t)pctl; // msg_control -> validated PRIVATE copy
                    *(uint64_t *)(ph + 40) = ccap;           // msg_controllen, clamped to the control window
                } else {
                    *(uint64_t *)(ph + 32) = (uint64_t)(R->buf + coff); // recvmsg: ring ptr (sentry writes fds here)
                    *(uint64_t *)(ph + 40) = clen;                      // msg_controllen, clamped to window
                }
            }
            *(uint32_t *)(ph + 48) = mflags;
            G_A1(&tmp) = (uint64_t)ph; // service_local reads/writes the PRIVATE msghdr
            msg_built = 1;
        }
    }

    if (bad) {
        R->ret = -EFAULT;
        R->nserved++;
        return;
    }

    // ---- per-process VIRTUAL fd translation (P1): map every guest fd ARGUMENT to its real sentry fd. A guest
    //      fd not in THIS process's table (a sentry-internal fd, another guest's fd, a stale fd) translates to
    //      -EBADF and never reaches the kernel. close + dup3 also mutate the table here (handled fully, then
    //      short-circuit); fds the call CREATES are virtualized on the OUT-path after service_local. ----
    static __thread uint8_t psel_save[3][128]; // pselect: saved ORIGINAL virtual fd_sets, for the result remap
    static __thread uint32_t psel_nfds;        // pselect: guest nfds (bounded to the table)
    static __thread uint8_t psel_present[3];   // pselect: which of rd/wr/ex sets were supplied
    // ppoll: bit k set = pollfd[k] named a POSITIVE virtual fd that is not mapped (stale/closed) -> the
    // OUT-path reports POLLNVAL for it (Linux), rather than the kernel silently ignoring a -1 entry.
    static __thread uint8_t poll_nval[SENTRY_DATACAP / 8u / 8u + 1u];
    int handled_local = 0;
    int64_t local_ret = 0;
    {
        pthread_mutex_lock(&g_fd_lock);
        struct sentry_proc *p = binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
        int eb = (p == NULL);
        g_bound_source_native = 0;
        g_bound_second_native = 0;
        if (p && (fd_in_a0(snr) || snr == 48 || snr == 56 || snr == 78 || snr == 79 || snr == 291 || snr == 439)) {
            int v = (int)(int64_t)G_A0(&tmp);
            if (v >= 0 && (uint32_t)v < SENTRY_VFD_MAX && p->real[v] >= 0 && !p->typed[v]) g_bound_source_native = 1;
        }
        if (p) switch (snr) {
            case 71: { // sendfile: a0=output, a1=input; both are virtual descriptors
                int output = (int)(int64_t)G_A0(&tmp);
                int input = (int)(int64_t)G_A1(&tmp);
                int real_output = vfd_real(p, output);
                int real_input = vfd_real(p, input);
                if (real_output < 0 || real_input < 0) {
                    eb = 1;
                } else {
                    g_bound_source_native = !p->typed[output];
                    g_bound_second_native = !p->typed[input];
                    G_A0(&tmp) = (uint64_t)(int64_t)real_output;
                    G_A1(&tmp) = (uint64_t)(int64_t)real_input;
                }
                break;
            }
            case 48:
            case 56:
            case 78:
            case 79:
            case 291:
            case 439: { // *at path operations: a0 = dirfd; AT_FDCWD (<0) passes through
                int path_fd = vfd_proc_path(p, (char *)G_A1(&tmp), SENTRY_PATHCAP);
                if (path_fd < 0) {
                    handled_local = 1;
                    local_ret = -ENOENT;
                    break;
                }
                if (path_fd == 2) g_bound_source_native = 1;
                int d = (int)(int64_t)G_A0(&tmp);
                if (d >= 0) {
                    int r = vfd_real(p, d);
                    if (r < 0)
                        eb = 1;
                    else {
                        char *relative = (char *)G_A1(&tmp);
                        int procfd_directory = p->procfd_dir[d];
                        if (!procfd_directory && relative[0] != '/') {
                            char descriptor_path[64];
                            char backing[HL_LINUX_PATH_MAX + 1];
                            int descriptor_length =
                                snprintf(descriptor_path, sizeof descriptor_path, "/proc/self/fd/%d", r);
                            ssize_t backing_length =
                                descriptor_length > 0 && (size_t)descriptor_length < sizeof descriptor_path
                                    ? readlink(descriptor_path, backing, sizeof backing - 1u)
                                    : -1;
                            if (backing_length > 0) {
                                backing[backing_length] = 0;
                                procfd_directory = strstr(backing, "/.hl-proc-fd") != NULL;
                            }
                        }
                        if (relative[0] != '/' && procfd_directory) {
                            char joined[SENTRY_PATHCAP];
                            int length = snprintf(joined, sizeof joined, "/proc/self/fd/%s", relative);
                            if (length < 0 || (size_t)length >= sizeof joined) {
                                handled_local = 1;
                                local_ret = -ENAMETOOLONG;
                                break;
                            }
                            memcpy(relative, joined, (size_t)length + 1u);
                            int translated = vfd_proc_path(p, relative, SENTRY_PATHCAP);
                            if (translated < 0) {
                                handled_local = 1;
                                local_ret = -ENOENT;
                                break;
                            }
                            if (translated == 2) g_bound_source_native = 1;
                            G_A0(&tmp) = (uint64_t)(int64_t)-100;
                            break;
                        }
                        const char *directory = r < HL_NFD ? g_fdpath[r] : NULL;
                        if (relative[0] != '/' && directory != NULL && directory[0]) {
                            if (g_rootfs && !strncmp(directory, g_rootfs_canon, g_rootfs_canon_len))
                                directory += g_rootfs_canon_len;
                            if (!strncmp(directory, "/proc/", 6) || !strcmp(directory, "/proc") ||
                                !strncmp(directory, "/dev/fd", 7)) {
                                char joined[SENTRY_PATHCAP];
                                int length = snprintf(joined, sizeof joined, "%s/%s", directory, relative);
                                if (length < 0 || (size_t)length >= sizeof joined) {
                                    handled_local = 1;
                                    local_ret = -ENAMETOOLONG;
                                    break;
                                }
                                memcpy(relative, joined, (size_t)length + 1u);
                                G_A0(&tmp) = (uint64_t)(int64_t)-100;
                                break;
                            }
                        }
                        G_A0(&tmp) = (uint64_t)(int64_t)r;
                    }
                }
                break;
            }
            case 57: { // close: translate + drop the mapping. A BORROWED (stdio) fd is unmapped but NOT closed.
                int v = (int)(int64_t)G_A0(&tmp);
                int r = vfd_real(p, v);
                if (r < 0) {
                    eb = 1;
                    break;
                }
                int typed = p->typed[v];
                if (vfd_drop(p, v) < 0) {
                    handled_local = 1;
                    local_ret = 0;
                    break;
                } // borrowed: success, real fd stays
                sentry_owned_close(r, typed);
                handled_local = 1;
                local_ret = 0;
                break;
            }
            case 24: { // dup3(oldfd, newfd, flags): handled ENTIRELY here -- never let the kernel use the guest's
                       //   virtual newfd as a real target. dup the real oldfd, then bind the guest's chosen virtual
                       //   newfd to the result (closing whatever it named). (fscache flush is skipped -- a pure
                       //   fd-table op.)
                int oldv = (int)(int64_t)G_A0(&tmp), newv = (int)(int64_t)G_A1(&tmp), flags = (int)G_A2(&tmp);
                int rold = vfd_real(p, oldv);
                if (rold < 0) {
                    eb = 1;
                    break;
                }
                handled_local = 1;
                if (oldv == newv) {
                    local_ret = -EINVAL;
                    break;
                } // Linux dup3 EINVAL on equal fds
                if (newv < 0 || (uint32_t)newv >= SENTRY_VFD_MAX) {
                    local_ret = -EBADF;
                    break;
                }
                int rnew;
                if (p->typed[oldv]) {
                    hl_linux_fd_snapshot typed;
                    if (!bound_snapshot((uint64_t)(uint32_t)rold, &typed)) {
                        local_ret = -EBADF;
                        break;
                    }
                    rnew = (int)bound_dup_at_least(typed.fd, 0, (flags & LX_O_CLOEXEC) ? HL_LINUX_FD_CLOEXEC : 0);
                } else {
                    rnew = fcntl(rold, (flags & O_CLOEXEC) ? F_DUPFD_CLOEXEC : F_DUPFD, 0);
                }
                if (rnew < 0) {
                    local_ret = -errno;
                    break;
                }
                int prev_typed = p->typed[newv];
                int prev = vfd_drop(p, newv);
                if (prev >= 0) sentry_owned_close(prev, prev_typed);
                p->real[newv] = rnew;
                p->borrowed[newv] = 0;
                p->typed[newv] = p->typed[oldv];
                p->procfd_dir[newv] = p->procfd_dir[oldv];
                p->cloexec[newv] = (flags & LX_O_CLOEXEC) != 0; // dup3 sets FD_CLOEXEC iff LX_O_CLOEXEC given
                local_ret = newv;
                break;
            }
            case 76:
            case 285: { // splice/copy_file_range(fd_in=a0, fd_out=a2): translate BOTH virtual descriptors
                int r0 = vfd_real(p, (int)(int64_t)G_A0(&tmp));
                int r2 = vfd_real(p, (int)(int64_t)G_A2(&tmp));
                if (r0 < 0 || r2 < 0)
                    eb = 1;
                else {
                    G_A0(&tmp) = (uint64_t)(int64_t)r0;
                    G_A2(&tmp) = (uint64_t)(int64_t)r2;
                }
                break;
            }
            case 21: { // epoll_ctl(epfd, op, fd, ev): translate BOTH the epoll fd (a0) and the target fd (a2)
                int r0 = vfd_real(p, (int)(int64_t)G_A0(&tmp));
                int r2 = vfd_real(p, (int)(int64_t)G_A2(&tmp));
                if (r0 < 0 || r2 < 0)
                    eb = 1;
                else {
                    G_A0(&tmp) = (uint64_t)(int64_t)r0;
                    G_A2(&tmp) = (uint64_t)(int64_t)r2;
                }
                break;
            }
            case 73: { // ppoll: translate each pollfd.fd (8B/entry, fd at +0) in the ring array to its real fd
                uint32_t nfds = (uint32_t)G_A1(&tmp);
                memset(poll_nval, 0, sizeof poll_nval);
                for (uint32_t k = 0; k < nfds; k++) {
                    int *fdp = (int *)(R->buf + (size_t)k * 8u);
                    int ofd = *fdp;
                    int r = vfd_real(p, ofd);
                    // A POSITIVE fd the sentry never handed this guest is stale/closed -> Linux reports
                    // POLLNVAL for it (remembered here, applied on the OUT-path). A NEGATIVE fd is a
                    // caller-requested ignore and legitimately polls as -1 (revents 0).
                    if (r < 0 && ofd >= 0 && k < sizeof(poll_nval) * 8u) poll_nval[k >> 3] |= (uint8_t)(1u << (k & 7));
                    *fdp = (r < 0) ? -1 : r; // never forward a wrong fd
                }
                break;
            }
            case 72: { // pselect6: rebuild REAL fd_sets from the virtual ones in place; save the originals so the
                       //   result can be remapped back to virtual fds on the OUT-path
                uint32_t nfds = (uint32_t)G_A0(&tmp);
                if (nfds > SENTRY_VFD_MAX) nfds = SENTRY_VFD_MAX;
                psel_nfds = nfds;
                uint8_t *win[3] = {R->buf + SENTRY_PSEL_RD, R->buf + SENTRY_PSEL_WR, R->buf + SENTRY_PSEL_EX};
                psel_present[0] = (uint8_t)have[1];
                psel_present[1] = (uint8_t)have[2];
                psel_present[2] = (uint8_t)have[3];
                int maxreal = -1;
                for (int s = 0; s < 3; s++) {
                    if (!psel_present[s]) continue;
                    memcpy(psel_save[s], win[s], 128); // stash the ORIGINAL virtual set
                    memset(win[s], 0, 128);            // rebuild it as the REAL set
                    for (uint32_t v = 0; v < nfds; v++) {
                        if (!(psel_save[s][v >> 3] & (1u << (v & 7)))) continue;
                        int r = vfd_real(p, (int)v);
                        if (r < 0) {
                            eb = 1; // Linux select/pselect: an invalid fd in any set -> EBADF, not a silent skip
                            break;
                        }
                        if ((uint32_t)r >= 1024u) continue; // unrepresentable in the real fd_set -> not selectable
                        win[s][r >> 3] |= (uint8_t)(1u << (r & 7));
                        if (r > maxreal) maxreal = r;
                    }
                    if (eb) break;
                }
                G_A0(&tmp) = (uint64_t)(maxreal + 1); // real nfds
                break;
            }
            default:
                if (fd_in_a0(snr)) {
                    int r = vfd_real(p, (int)(int64_t)G_A0(&tmp));
                    if (r < 0)
                        eb = 1;
                    else
                        G_A0(&tmp) = (uint64_t)(int64_t)r;
                }
                break;
            }
        pthread_mutex_unlock(&g_fd_lock);
        if (eb) {
            R->ret = -EBADF;
            R->nserved++;
            return;
        }
        if (handled_local) {
            R->ret = local_ret;
            R->nserved++;
            return;
        }
    }

    service_local(&tmp); // real host authority + container policy (touches only ring + private memory now)
    int64_t ret = (int64_t)G_RET(&tmp);
    R->ret = ret;

    // Mirror PRIVATE out-values back into the ring so the worker's copy-back into guest memory sees them.
    if (slen_back) *(socklen_t *)(R->buf + SENTRY_SLEN_OFF) = pslen;
    if (msg_built && snr == 212) {
        *(uint32_t *)(R->buf + 8) = *(uint32_t *)(ph + 8);   // updated msg_namelen
        *(uint64_t *)(R->buf + 40) = *(uint64_t *)(ph + 40); // updated msg_controllen
        *(uint32_t *)(R->buf + 48) = *(uint32_t *)(ph + 48); // updated msg_flags
    }

    // ---- VIRTUALIZE newly-created fds (P1): every real fd service_local just produced is mapped to a fresh
    //      per-process virtual fd, so the worker only ever sees virtual numbers. Also remap pselect's narrowed
    //      result fd_sets back to the guest's virtual fd positions. (close drops its mapping on the IN-path;
    //      dup3 is fully handled there.) ----
    {
        pthread_mutex_lock(&g_fd_lock);
        struct sentry_proc *p = binding_table_locked((pid_t)R->wpid, R->wtid, R->inherit_wtid, 1);
        if (p) switch (snr) {
            case 56:
            case 198:
            case 202:
            case 242:
            case 19:
            case 23:
            case 279: // memfd_create: an anonymous sentry-owned file enters the virtual table like an open
            case 20:  // openat/socket/accept*/dup/eventfd2/epoll_create1
                if (ret >= 0) {
                    int v = vfd_alloc(p, (int)ret, 0);
                    if (v < 0) {
                        sentry_created_close((int)ret);
                        R->ret = -EMFILE;
                    } else {
                        // Track the guest's FD_CLOEXEC intent so a later guest execve sweeps this fd. The
                        // CLOEXEC bit (O_CLOEXEC == SOCK_CLOEXEC == EFD_CLOEXEC == EPOLL_CLOEXEC == 0x80000)
                        // rides a different arg per syscall; dup(23)/accept(202) never set it.
                        int cx = 0;
                        switch (snr) {
                        case 56: cx = (R->a[2] & LX_O_CLOEXEC) != 0; break;  // openat flags
                        case 198: cx = (R->a[1] & LX_O_CLOEXEC) != 0; break; // socket type
                        case 242: cx = (R->a[3] & LX_O_CLOEXEC) != 0; break; // accept4 flags
                        case 19: cx = (R->a[1] & LX_O_CLOEXEC) != 0; break;  // eventfd2 flags
                        case 20: cx = (R->a[0] & LX_O_CLOEXEC) != 0; break;  // epoll_create1 flags
                        case 279: cx = (R->a[1] & 1u) != 0; break;           // memfd_create MFD_CLOEXEC
                        default: cx = 0; break;                              // dup(23) / accept(202)
                        }
                        p->cloexec[v] = (uint8_t)cx;
                        if (snr == 56) {
                            const char *opened = (const char *)G_A1(&tmp);
                            p->procfd_dir[v] = opened != NULL && !strcmp(opened, "/proc/self/fd");
                        }
                        R->ret = v;
                    }
                }
                break;
            case 25: // fcntl F_DUPFD(0)/F_DUPFD_CLOEXEC(1030): the result is a new real fd -> virtualize it,
                     //   honoring the guest's minimum-fd hint (a2, a virtual lower bound)
                if ((G_A1(&tmp) == 0 || G_A1(&tmp) == 1030) && ret >= 0) {
                    uint32_t minv = (uint32_t)R->a[2];
                    int v = vfd_alloc(p, (int)ret, minv < SENTRY_VFD_MAX ? minv : 0);
                    if (v < 0) {
                        sentry_created_close((int)ret);
                        R->ret = -EMFILE;
                    } else {
                        p->cloexec[v] = (G_A1(&tmp) == 1030); // F_DUPFD_CLOEXEC sets FD_CLOEXEC on the new fd
                        int source_vfd = (int)(int64_t)R->a[0];
                        if (source_vfd >= 0 && (uint32_t)source_vfd < SENTRY_VFD_MAX) {
                            p->typed[v] = p->typed[source_vfd];
                            p->procfd_dir[v] = p->procfd_dir[source_vfd];
                        }
                        R->ret = v;
                    }
                } else if (G_A1(&tmp) == 2 /* F_SETFD */) {
                    // Track FD_CLOEXEC on the guest's virtual fd (the real sentry fd's flag is irrelevant to a
                    // guest execve, which is a local image reload). Serve success without a real-fd flag change.
                    int v = (int)(int64_t)R->a[0];
                    if (v >= 0 && (uint32_t)v < SENTRY_VFD_MAX && p->real[v] >= 0) {
                        p->cloexec[v] = (R->a[2] & 1 /* FD_CLOEXEC */) != 0;
                        R->ret = 0;
                    }
                } else if (G_A1(&tmp) == 1 /* F_GETFD */) {
                    // Return the guest's tracked FD_CLOEXEC, not the real sentry fd's.
                    int v = (int)(int64_t)R->a[0];
                    if (v >= 0 && (uint32_t)v < SENTRY_VFD_MAX && p->real[v] >= 0) R->ret = p->cloexec[v] ? 1 : 0;
                }
                break;
            case 59:
            case 199: // pipe2 / socketpair: two real fds at buf[0..8) -> virtualize both in place
                if (ret == 0) {
                    int r0 = *(int *)(R->buf), r1 = *(int *)(R->buf + 4);
                    int v0 = vfd_alloc(p, r0, 0), v1 = (v0 >= 0) ? vfd_alloc(p, r1, 0) : -1;
                    if (v0 < 0 || v1 < 0) {
                        if (v0 >= 0) vfd_drop(p, v0);
                        sentry_native_close(r0);
                        sentry_native_close(r1);
                        R->ret = -EMFILE;
                    } else {
                        // pipe2(fds,flags): flags=a1; socketpair(dom,type,proto,fds): SOCK_CLOEXEC rides type=a1.
                        uint8_t cx = (R->a[1] & LX_O_CLOEXEC) != 0;
                        p->typed[v0] = 0;
                        p->typed[v1] = 0;
                        p->cloexec[v0] = cx;
                        p->cloexec[v1] = cx;
                        *(int *)(R->buf) = v0;
                        *(int *)(R->buf + 4) = v1;
                    }
                }
                break;
            case 212: // recvmsg: virtualize any SCM_RIGHTS real fds the sentry received in the control window
                if (ret >= 0 && coff) sentry_cmsg_translate_in(p, R->buf + coff, (size_t)*(uint64_t *)(R->buf + 40));
                break;
            case 73: // ppoll: stamp POLLNVAL(0x20) into revents(+6,2B) for each entry that named a stale/closed
                     //   positive virtual fd (marked on the IN-path). The kernel returned revents 0 for the -1
                     //   we substituted; Linux reports POLLNVAL so an event loop notices the invalidation. A
                     //   POLLNVAL entry also counts toward the ready-fd return value.
            {
                uint32_t nf = (uint32_t)R->a[1];
                for (uint32_t k = 0; k < nf; k++) {
                    if (!(poll_nval[k >> 3] & (1u << (k & 7)))) continue;
                    uint16_t *rev = (uint16_t *)(R->buf + (size_t)k * 8u + 6u);
                    if (!(*rev & 0x20u)) {
                        *rev |= 0x20u; // POLLNVAL
                        if (R->ret >= 0) R->ret++;
                    }
                }
            } break;
            case 72: // pselect6: remap the kernel-narrowed REAL fd_sets back to the guest's VIRTUAL fd positions
                if (ret >= 0) {
                    uint8_t *win[3] = {R->buf + SENTRY_PSEL_RD, R->buf + SENTRY_PSEL_WR, R->buf + SENTRY_PSEL_EX};
                    for (int s = 0; s < 3; s++) {
                        if (!psel_present[s]) continue;
                        uint8_t out[128];
                        memset(out, 0, sizeof out);
                        for (uint32_t v = 0; v < psel_nfds; v++) {
                            if (!(psel_save[s][v >> 3] & (1u << (v & 7)))) continue; // only originally-requested fds
                            int r = vfd_real(p, (int)v);
                            if (r < 0 || (uint32_t)r >= 1024u) continue;
                            if (win[s][r >> 3] & (1u << (r & 7))) out[v >> 3] |= (uint8_t)(1u << (v & 7));
                        }
                        memcpy(win[s], out, 128); // worker copies the window -> guest fd_set
                    }
                }
                break;
            default: break;
            }
        pthread_mutex_unlock(&g_fd_lock);
    }
    R->nserved++;
}

// One servicer thread per ring: spin for a request, service it, hand the ring back. The orphan-guard
// and the shared quit flag both _exit() the WHOLE sentry process (killing every servicer thread).
static void sentry_ring_loop(struct sentry_ring *R) {
    for (;;) {
        uint32_t spins = 0;
        uint32_t idle_rounds = 0; // yield rounds since the last serviced request (resets per request)
        while (atomic_load_explicit(&R->turn, memory_order_acquire) != 1 ||
               atomic_load_explicit(&R->request, memory_order_acquire) ==
                   atomic_load_explicit(&R->response, memory_order_acquire)) {
            if (atomic_load_explicit(&g_shm->quit, memory_order_acquire)) _exit(0);
            if (++spins > 256) {
                if (getppid() == 1) _exit(0); // orphan-guard: worker died/crashed -> don't spin forever
                // A quiet lane must not burn a core forever: with a 64-lane pool most lanes are idle most
                // of the time, so after ~1k yield rounds fall back to a real sleep. A newly armed turn is
                // still observed within ~100us -- negligible against a forwarded syscall's round-trip --
                // and a BUSY lane (request in flight or back-to-back traffic) never reaches the sleep.
                if (++idle_rounds > 1024) {
                    struct timespec nap = {0, 100000}; // 100us
                    nanosleep(&nap, NULL);
                } else {
                    sched_yield();
                }
                spins = 0;
            }
        }
        uint64_t request = atomic_load_explicit(&R->request, memory_order_acquire);
        sentry_service_one(R);
        atomic_store_explicit(&R->turn, 0, memory_order_release); // hand back to the worker
        atomic_store_explicit(&R->response, request, memory_order_release);
    }
}

static void *sentry_ring_thread(void *p) {
    sentry_ring_loop((struct sentry_ring *)p);
    return NULL; // unreachable (loop _exit()s)
}

// The sentry process body: ONE process (so all servicers share the host fd table) running N servicer
// threads -- one per ring. Spawns N-1 threads for ring[1..N-1] and services ring[0] on the main thread.
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
    pid_t pid = fork(); // sentry forks AFTER load -> inherits the fd table / jail config / auxv / cwd
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
    uint64_t total = 0;
    for (int i = 0; i < SENTRY_NRINGS; i++)
        total += g_shm->ring[i].nserved;
    char message[96];
    int length = snprintf(message, sizeof message, "[sentry] forwarded %llu syscalls; sentry reaped\n",
                          (unsigned long long)total);
    if (length > 0 && (size_t)length < sizeof message)
        (void)hl_sentry_pipe_write(STDERR_FILENO, message, (size_t)length);
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
static int sentry_worker_proc_leaf(const char *path) {
    static const char *const leaves[] = {"auxv",   "maps",      "smaps",   "stat",        "status",
                                         "statm",  "environ",   "cmdline", "comm",        "exe",
                                         "limits", "mountinfo", "pagemap", "task/1/maps", NULL};
    if (path == NULL || strncmp(path, "/proc/", 6) != 0) return 0;
    const char *rest = path + 6;
    // "/proc/self" itself: glibc realpath("/proc/self/exe") readlinks every component, and the pid this
    // magic link names must be the WORKER's container pid, not the sentry's identity.
    if (strcmp(rest, "self") == 0) return 1;
    if (strncmp(rest, "self/", 5) == 0) {
        rest += 5;
    } else {
        int pid = 0;
        const char *s = rest;
        while (*s >= '0' && *s <= '9')
            pid = pid * 10 + (*s++ - '0');
        if (s == rest || *s != '/' || pid != container_pid()) return 0;
        rest = s + 1;
    }
    for (int i = 0; leaves[i]; i++)
        if (strcmp(rest, leaves[i]) == 0) return 1;
    return 0;
}

// Hand a worker-opened real fd to the sentry for adoption into this process's virtual fd table
// (SENTRY_OP_ADOPT). The fd datagram is queued on the control socketpair BEFORE the turn flips so the
// sentry's recv never blocks. Returns the new virtual fd, or a negated errno.
static int64_t sentry_adopt_fd(int rfd, int cloexec) {
    struct sentry_ring *R = ring_for_thread();
    while (atomic_exchange_explicit(&R->busy, 1, memory_order_acquire))
        sched_yield();
    int idx = t_ring;
    if (idx < 0 || g_ctl[idx][0] < 0) {
        atomic_store_explicit(&R->busy, 0, memory_order_release);
        return -EIO;
    }
    R->wpid = (uint32_t)g_worker_pid;
    R->wtid = t_token;
    R->inherit_wtid = 0;
    R->rawnr = SENTRY_OP_ADOPT;
    R->a[0] = (uint64_t)(cloexec != 0);
    R->iovn = 0;
    for (int i = 0; i < 6; i++)
        R->redir[i] = -1;
    sentry_send_fd(g_ctl[idx][0], rfd);
    uint64_t request = atomic_fetch_add_explicit(&R->request, 1, memory_order_relaxed) + 1;
    atomic_store_explicit(&R->turn, 1, memory_order_release);
    uint32_t sp = 0;
    while (atomic_load_explicit(&R->response, memory_order_acquire) != request)
        if (++sp > 256) {
            sched_yield();
            sp = 0;
        }
    int64_t ret = R->ret;
    atomic_store_explicit(&R->busy, 0, memory_order_release);
    return ret;
}

// ------------------------------------------------------------------ the routed trust boundary
// Replaces the direct service_local(c) call for untrusted guests. When g_untrusted is off this is a
// transparent pass-through (trusted path byte-identical to baseline -- and service() already gated us
// out before getting here). When on, fs/net/proc syscalls are marshaled to the sentry over the ring;
// everything else stays local in the worker.
static void syscall_route(struct cpu *c) {
    if (!g_untrusted) {
        service_local(c);
        return;
    }
    // Normalize legacy x86 forms (open->openat, ...) in the worker so we classify by canonical number;
    // a return of 1 means it was fully handled locally (arch_prctl/TLS) -- it must stay here.
    if (G_NORMALIZE(c)) return;
    uint64_t nr = G_NR(c);

    /* service_local rebases pointer arguments from a biased ET_EXEC's Linux link range to the host mapping.
     * A FORWARDED call is marshaled before service_local runs, so apply the same table here -- the same
     * table, from nonpie_args.h, restricted to what we forward, because anything else reaches service_local
     * and gets it there. (Two independently maintained copies of this list is what let static x86 strings
     * and buffers be copied from their unmapped low link addresses: empty paths, zero-filled pipe writes.)
     * The fold is idempotent, so a forwarded call that also falls through to service_local is unharmed. */
    if (g_nonpie_lo && sentry_forwarded(nr)) {
        uint64_t reb[6] = {G_A0(c), G_A1(c), G_A2(c), G_A3(c), G_A4(c), G_A5(c)};
        nonpie_rebase_args(nr, reb);
        G_A0(c) = reb[0];
        G_A1(c) = reb[1];
        G_A2(c) = reb[2];
        G_A3(c) = reb[3];
        G_A4(c) = reb[4];
        G_A5(c) = reb[5];
        // No nonpie_rebase_iov here: readv/writev copy the payload into the ring instead of handing the
        // array to a host syscall, and fold each iov_base inside the flatten (case 65/66 below).
    }

    // exit(93)/exit_group(94): service_local() never returns.  exit(93) also ends the PROCESS when this is
    // its last thread, so release the process table in that case before entering the host syscall.  Missing
    // that distinction leaves the last child's duplicated pipe writers alive in the sentry forever.
    if (nr == 93 || nr == 94) {
        int process_exit = nr == 94 || atomic_fetch_sub(&g_worker_threads, 1) == 1;
        if (process_exit) {
            // exit_group ends the PROCESS. A forked CHILD worker releases its OWN sentry-side fd table (closing
            // its inherited/owned real fds); the OWNER tears the whole sentry down (reclaiming everything).
            if (getpid() != g_sentry_owner_pid) sentry_process_release();
            sentry_shutdown();
        } else if (t_ring >= 0) {
            sentry_ctl_op(SENTRY_OP_THREAD_EXIT, 0, 0);
        }
        ring_release();
        service_local(c);
        return;
    }

    // --- fork/exec/wait lane (item 1) -------------------------------------------------------------------
    // clone(220)/clone3(435): a guest THREAD is a host pthread (stays this process; gets its own lane
    // lazily). A guest FORK is a real worker fork() (the guest address space is worker-side COW memory the
    // sentry cannot duplicate) done LOCALLY by service_local. The freshly forked CHILD inherited the
    // parent's lane + sentry-ownership, so re-init its bookkeeping; the PARENT counts the new child so a
    // later wait4 with no real children doesn't deadlock on the hidden sentry child.
    if (nr == 220 || nr == 435) {
        uint64_t clone3_flags = 0;
        if (nr == 435 && G_A0(c) &&
            guest_copy_from(&clone3_flags, G_A0(c), sizeof clone3_flags) != sizeof clone3_flags) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            return;
        }
        int is_thread = (nr == 220) ? ((G_A0(c) & 0x10000) != 0) : ((clone3_flags & 0x10000) != 0);
        int64_t fork_snapshot = is_thread ? 0 : sentry_ctl_op(SENTRY_OP_FORK_PREPARE, 0, 0);
        if (!is_thread && fork_snapshot < 0) {
            G_RET(c) = (uint64_t)fork_snapshot;
            return;
        }
        int fork_sync[2] = {-1, -1};
        if (!is_thread && pipe(fork_sync) != 0) {
            int error = errno;
            sentry_ctl_op(SENTRY_OP_FORK_CANCEL, (uint64_t)fork_snapshot, 0);
            G_RET(c) = (uint64_t)(int64_t)-error;
            return;
        }
        service_local(c);               // spawn_thread (CLONE_THREAD) or fork() -- both worker-local
        if (getpid() != g_worker_pid) { // we are the new child worker
            close(fork_sync[1]);
            unsigned char ready;
            ssize_t received;
            do
                received = read(fork_sync[0], &ready, sizeof ready);
            while (received < 0 && errno == EINTR);
            close(fork_sync[0]);
            if (received != sizeof ready || ready != 1) _exit(127);
            sentry_fork_child(); // drop inherited lane, mint a fresh token + identity
            return;
        }
        if (!is_thread && (int64_t)G_RET(c) > 0) {
            close(fork_sync[0]);
            pid_t child = (pid_t)G_RET(c);
            int64_t installed = sentry_ctl_op(SENTRY_OP_FORK, (uint64_t)fork_snapshot, (uint64_t)child);
            if (installed < 0) {
                sentry_ctl_op(SENTRY_OP_FORK_CANCEL, (uint64_t)fork_snapshot, 0);
                close(fork_sync[1]);
                kill(child, SIGKILL);
                int status;
                while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
                G_RET(c) = (uint64_t)installed;
                return;
            }
            unsigned char ready = 1;
            ssize_t written = hl_sentry_pipe_write(fork_sync[1], &ready, sizeof ready);
            close(fork_sync[1]);
            if (written != sizeof ready) {
                kill(child, SIGKILL);
                int status;
                while (waitpid(child, &status, 0) < 0 && errno == EINTR) {}
                sentry_ctl_op(SENTRY_OP_REAP, (uint64_t)child, 0);
                G_RET(c) = (uint64_t)(int64_t)-EIO;
                return;
            }
            atomic_fetch_add(&g_guest_children, 1); // parent: a child appeared
        } else if (!is_thread) {
            close(fork_sync[0]);
            close(fork_sync[1]);
            sentry_ctl_op(SENTRY_OP_FORK_CANCEL, (uint64_t)fork_snapshot, 0);
        }
        return;
    }
    // execve(221) stays LOCAL: service_local reloads the guest image IN THIS PROCESS (it is not a host
    // execve), so the worker keeps its pid, ring lane, control sockets, sentry, and confinement across it.
    // But because it is NOT a real execve, the kernel never applies FD_CLOEXEC: ask the sentry to close+drop
    // this worker's cloexec virtual fds first, so a guest that set FD_CLOEXEC before exec sees them gone
    // (pipe EOF for the peer, no leaked resources) exactly as on Linux. Only on a SUCCESSFUL exec — if the
    // image load fails (service_local returns with a negated errno in the return reg) the fds must survive.
    if (nr == 221) {
        int64_t bound = sentry_ctl_op(SENTRY_OP_BIND, 0, 0);
        if (bound < 0) {
            G_RET(c) = (uint64_t)bound;
            return;
        }
        service_local(c);
        // execve sets c->redirect on SUCCESS (the tail must not advance PC into the new image); a FAILED
        // execve (ENOENT/EACCES/…) leaves redirect clear and returns -errno, in which case the fds must
        // survive. Only on success sweep this worker's cloexec virtual fds sentry-side.
        if (c->redirect) sentry_ctl_op(SENTRY_OP_EXEC, (uint64_t)(uint32_t)g_worker_pid, 0);
        return;
    }
    // openat(56)/readlinkat(78) of a per-process guest-state /proc file: serve it LOCALLY -- only this
    // worker holds the current image's identity (the sentry's copy is the pre-fork/pre-exec one; see
    // sentry_worker_proc_leaf). readlinkat is pure state (bytes into a guest buffer). openat yields a real
    // worker-local fd, which must not leak into the guest's (fully virtual) descriptor space -- hand it to
    // the sentry for adoption so read/lseek/close forward exactly like any sentry-opened descriptor.
    char worker_proc_path[SENTRY_PATHCAP];
    int worker_proc_path_len = ((nr == 56 || nr == 78) && G_A1(c))
                                   ? guest_copy_string(worker_proc_path, sizeof worker_proc_path, G_A1(c))
                                   : -1;
    if (worker_proc_path_len >= 0 && sentry_worker_proc_leaf(worker_proc_path)) {
        service_local(c);
        if (nr == 56 && (int64_t)G_RET(c) >= 0) {
            int rfd = (int)G_RET(c);
            int64_t v = sentry_adopt_fd(rfd, (G_A2(c) & LX_O_CLOEXEC) != 0);
            sentry_native_close(rfd);
            G_RET(c) = (uint64_t)v;
        }
        return;
    }
    // wait4(260): reap the guest's child WORKER processes locally. The sentry is ALSO a child of the owner,
    // so a blocking wait-any with no GUEST children would hang on it -> short-circuit to -ECHILD; and never
    // surface the sentry's own pid to the guest. A specific-pid wait passes straight through.
    if (nr == 260) {
        int64_t wpid = (int64_t)(int)G_A0(c);
        if (wpid <= 0 && atomic_load(&g_guest_children) <= 0) {
            G_RET(c) = (uint64_t)(-ECHILD);
            return;
        }
        service_local(c);
        int64_t r = (int64_t)G_RET(c);
        if (r > 0) {
            if (g_sentry_pid && r == (int64_t)g_sentry_pid) {
                G_RET(c) = (uint64_t)(-ECHILD);
                return;
            }
            // A normally exiting worker sends SENTRY_OP_EXIT itself. A worker killed by a signal cannot
            // run that cleanup, so its sentry-owned descriptor copy would otherwise remain live forever
            // (notably keeping pipe writers open after waitpid returned). A successful wait without
            // WUNTRACED/WCONTINUED necessarily reaped a terminated child. With either reporting option,
            // inspect the Linux status when supplied; for a NULL status, a reaped pid no longer exists.
            int terminated = (G_A2(c) & (2u | 8u)) == 0;
            if (!terminated && G_A1(c)) {
                int status;
                if (guest_copy_from(&status, G_A1(c), sizeof status) == sizeof status)
                    terminated = (status & 0xff) != 0x7f && status != 0xffff;
            } else if (!terminated && !G_A1(c)) {
                errno = 0;
                terminated = kill((pid_t)r, 0) < 0 && errno == ESRCH;
            }
            if (terminated) {
                sentry_ctl_op(SENTRY_OP_REAP, (uint64_t)(uint32_t)r, 0);
                atomic_fetch_sub(&g_guest_children, 1);
            }
        }
        return;
    }
    // file-backed mmap(222): the mapping must live in the WORKER (memory authority) but the fd is
    // sentry-owned and invalid here. Borrow the real fd over this lane's control socket (SCM_RIGHTS), map
    // it locally with the borrowed number, then drop it -- so the worker holds the real fd only for the
    // single mmap. Anonymous mmap (MAP_ANON 0x20) needs no fd and stays fully local below.
    if (nr == 222 && !(G_A3(c) & 0x20) && (int)G_A4(c) >= 0) {
        struct sentry_ring *R = ring_for_thread();
        while (atomic_exchange_explicit(&R->busy, 1, memory_order_acquire))
            sched_yield();
        int idx = t_ring;
        R->wpid = (uint32_t)g_worker_pid; // select this process's table: the guest VFD is translated there
        R->wtid = t_token;
        R->inherit_wtid = 0;
        R->rawnr = SENTRY_OP_FDPASS;
        R->a[0] = (uint64_t)(uint32_t)(int)G_A4(c); // the guest's (virtual) mmap fd
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
        // The sentry ALWAYS sends a control datagram (with the fd, or empty on -EBADF), so we MUST always
        // receive it -- skipping on failure would leave a stale message to desync the next lend on this lane.
        int lfd = (idx >= 0 && g_ctl[idx][0] >= 0) ? sentry_recv_fd(g_ctl[idx][0]) : -1;
        atomic_store_explicit(&R->busy, 0, memory_order_release);
        uint64_t saved = G_A4(c);
        G_A4(c) = (uint64_t)(int64_t)lfd; // -1 if the lend failed -> service_local mmap returns EBADF
        service_local(c);                 // real worker-side mmap on the borrowed fd
        G_A4(c) = saved;                  // restore the guest's r8/a4 (preserved across a syscall)
        if (lfd >= 0) close(lfd);         // drop the borrowed fd: worker fds stay virtual
        return;
    }

    if (!sentry_forwarded(nr)) {
        service_local(c); // LOCAL authority (its G_NORMALIZE re-runs as a no-op on already-*at registers)
        return;
    }

    struct sentry_ring *R = ring_for_thread(); // this worker thread's private ring (pool, keyed lazily)
    // Producer lock: at <=N concurrent worker threads each owns a distinct ring and this is an
    // uncontended single TAS; overflow threads (sharing a lane) serialize here, preserving the SPSC
    // ping-pong on the shared ring. Held across the whole round-trip + the output copy-back.
    while (atomic_exchange_explicit(&R->busy, 1, memory_order_acquire))
        sched_yield();
    R->wpid = (uint32_t)g_worker_pid; // stamp the worker PROCESS: selects this guest's virtual fd table (P1/P2)
    R->wtid = t_token;
    R->inherit_wtid = 0;
#ifdef G_PROF_EXTRA
    R->rawnr = hl_linux_syscall_guest_number(HL_LINUX_GUEST_X86_64, nr);
    if (R->rawnr == UINT64_MAX) {
        atomic_store_explicit(&R->busy, 0, memory_order_release);
        G_RET(c) = (uint64_t)(int64_t)-ENOSYS;
        return;
    }
#else
    R->rawnr = nr;
#endif
    R->a[0] = G_A0(c);
    R->a[1] = G_A1(c);
    R->a[2] = G_A2(c);
    R->a[3] = G_A3(c);
    R->a[4] = G_A4(c);
    R->a[5] = G_A5(c);
    for (int i = 0; i < 6; i++)
        R->redir[i] = -1;
    R->iovn = 0;
    R->inlen = 0;

    /*
     * Import every guest descriptor exactly once before publishing the ring.
     * These snapshots survive the synchronous round trip and are also used for
     * copy-back, closing both the sentry pointer escape and worker-side TOCTOU
     * window where an iovec/msghdr was formerly reread after the host call.
     */
    struct iovec worker_iov[SENTRY_IOVMAX];
    uint32_t worker_iovn = 0;
    uint8_t worker_msghdr[SENTRY_MSGHDR_SZ];
    struct iovec worker_msg_iov[SENTRY_IOVMAX];
    uint32_t worker_msg_iovn = 0;
    int worker_msghdr_valid = 0;
    socklen_t worker_socklen = 0;
    int worker_socklen_valid = 0;

#define SENTRY_IMPORT_EXACT(dst, src, len)                                                                             \
    do {                                                                                                               \
        size_t _n = (size_t)(len);                                                                                     \
        if (_n && guest_copy_from((dst), (uint64_t)(src), _n) != (ssize_t)_n) {                                        \
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);                                                                   \
            atomic_store_explicit(&R->busy, 0, memory_order_release);                                                  \
            return;                                                                                                    \
        }                                                                                                              \
    } while (0)
#define SENTRY_IMPORT_STRING(dst, cap, src)                                                                            \
    do {                                                                                                               \
        int _r = guest_copy_string((dst), (cap), (uint64_t)(src));                                                     \
        if (_r < 0) {                                                                                                  \
            G_RET(c) = (uint64_t)(int64_t)_r;                                                                          \
            atomic_store_explicit(&R->busy, 0, memory_order_release);                                                  \
            return;                                                                                                    \
        }                                                                                                              \
        R->inlen = (uint32_t)_r + 1u;                                                                                  \
    } while (0)
#define SENTRY_REQUIRE_WRITE(ptr, len)                                                                                 \
    do {                                                                                                               \
        size_t _n = (size_t)(len);                                                                                     \
        if (_n && guest_accessible_prefix((uint64_t)(ptr), _n, HL_LOGICAL_VMA_WRITE) != _n) {                          \
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);                                                                   \
            atomic_store_explicit(&R->busy, 0, memory_order_release);                                                  \
            return;                                                                                                    \
        }                                                                                                              \
    } while (0)

    switch (nr) {
    case 48:  // faccessat
    case 56:  // openat
    case 439: // faccessat2
    {         // dfd, a1=path: in-path
        if (!G_A1(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return;
        }
        SENTRY_IMPORT_STRING((char *)R->buf, SENTRY_PATHCAP, G_A1(c));
        R->redir[1] = 0;
        break;
    }
    case 279: { // memfd_create(a0=name, a1=flags): in-name
        if (!G_A0(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return;
        }
        SENTRY_IMPORT_STRING((char *)R->buf, SENTRY_PATHCAP, G_A0(c));
        R->redir[0] = 0;
        break;
    }
    case 78: { // readlinkat(dfd, a1=path, a2=buf, a3=size): in-path + bounded out-buffer
        if (!G_A1(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return;
        }
        SENTRY_IMPORT_STRING((char *)R->buf, SENTRY_PATHCAP, G_A1(c));
        R->redir[1] = 0;
        R->redir[2] = SENTRY_PATHCAP;
        uint32_t cap = SENTRY_BUFSZ - SENTRY_PATHCAP;
        if (R->a[3] > cap) R->a[3] = cap;
        break;
    }
    case 64:   // write(fd, a1=buf, a2=len)
    case 68: { // pwrite64(fd, a1=buf, a2=len, a3=off): copy the payload into the ring; cap to BUFSZ
        uint32_t n = G_A2(c) > SENTRY_BUFSZ ? SENTRY_BUFSZ : (uint32_t)G_A2(c);
        if (n) {
            ssize_t copied = guest_copy_from(R->buf, G_A1(c), n);
            if (copied <= 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                atomic_store_explicit(&R->busy, 0, memory_order_release);
                return;
            }
            n = (uint32_t)copied; /* Linux permits a short write up to the first inaccessible byte. */
        }
        R->inlen = n;
        R->redir[1] = 0;
        R->a[2] = n; // ship exactly n bytes; a short (p)write is legal -> guest loops
        break;
    }
    case 71: // sendfile(out, in, offset*, count): optional in/out offset
        if (G_A2(c)) {
            SENTRY_IMPORT_EXACT(R->buf, G_A2(c), sizeof(int64_t));
            R->redir[2] = 0;
        }
        break;
    case 76:    // splice(fd_in, off_in*, fd_out, off_out*, len, flags)
    case 285: { // copy_file_range(fd_in, off_in*, fd_out, off_out*, len, flags): both offsets optional in/out
        if (G_A1(c)) {
            SENTRY_IMPORT_EXACT(R->buf, G_A1(c), sizeof(int64_t));
            R->redir[1] = 0;
        }
        if (G_A3(c)) {
            SENTRY_IMPORT_EXACT(R->buf + sizeof(int64_t), G_A3(c), sizeof(int64_t));
            R->redir[3] = (int32_t)sizeof(int64_t);
        }
        break;
    }
    case 63:   // read(fd, a1=buf, a2=len)
    case 67:   // pread64(fd, a1=buf, a2=len, a3=off)
    case 61: { // getdents64(fd, a1=buf, a2=count): reserve the out window; cap to BUFSZ
        uint32_t n = G_A2(c) > SENTRY_BUFSZ ? SENTRY_BUFSZ : (uint32_t)G_A2(c);
        if (n) {
            size_t prefix = guest_accessible_prefix(G_A1(c), n, HL_LOGICAL_VMA_WRITE);
            if (!prefix) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                atomic_store_explicit(&R->busy, 0, memory_order_release);
                return;
            }
            n = (uint32_t)prefix;
        }
        R->redir[1] = 0;
        R->a[2] = n; // short read / partial getdents is legal -> guest loops
        break;
    }
    case 80: // fstat(fd, a1=statbuf): out-struct only
        SENTRY_REQUIRE_WRITE(G_A1(c), SENTRY_STATSZ);
        R->redir[1] = 0;
        break;
    case 79: { // newfstatat(dfd, a1=path, a2=statbuf, flags): in-path + out-struct (two-buffer)
        if (!G_A1(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return;
        }
        SENTRY_IMPORT_STRING((char *)R->buf, SENTRY_PATHCAP, G_A1(c));
        SENTRY_REQUIRE_WRITE(G_A2(c), SENTRY_STATSZ);
        R->redir[1] = 0;              // path     -> buf[0]
        R->redir[2] = SENTRY_PATHCAP; // statbuf -> buf[SENTRY_PATHCAP]; copied back below on success
        break;
    }
    case 291: { // statx(dfd, a1=path, a2=flags, a3=mask, a4=statxbuf): in-path + out-struct
        if (!G_A1(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return;
        }
        SENTRY_IMPORT_STRING((char *)R->buf, SENTRY_PATHCAP, G_A1(c));
        SENTRY_REQUIRE_WRITE(G_A4(c), SENTRY_STATXSZ);
        R->redir[1] = 0;              // path      -> buf[0]
        R->redir[4] = SENTRY_PATHCAP; // statxbuf -> buf[SENTRY_PATHCAP]
        break;
    }
    case 65:   // readv(fd, a1=iov, a2=iovcnt)
    case 66: { // writev(fd, a1=iov, a2=iovcnt): flatten the guest iovec into the ring
        // Layout in buf[]: a `struct iovec[n]` header (iov_base = buf-relative OFFSET) followed by the
        // scatter/gather data. For writev we gather the guest segments now; for readv we just reserve
        // the windows and scatter back after the round-trip. iov_base offsets are bounds-checked and
        // rebased to ring pointers by the sentry, so no guest pointer ever crosses.
        uint32_t n = (uint32_t)G_A2(c);
        if (n > SENTRY_IOVMAX) n = SENTRY_IOVMAX; // partial scatter/gather is legal -> guest loops
        const struct iovec *giov = worker_iov;
        if (n) {
            if (guest_iov_import(G_A1(c), n, worker_iov) < 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                atomic_store_explicit(&R->busy, 0, memory_order_release);
                return;
            }
            if (g_nonpie_lo)
                for (uint32_t i = 0; i < n; i++)
                    worker_iov[i].iov_base = (void *)(uintptr_t)nonpie_p((uint64_t)(uintptr_t)worker_iov[i].iov_base);
        }
        worker_iovn = n;
        struct iovec *biov = (struct iovec *)R->buf;
        uint32_t cur = n * (uint32_t)sizeof(struct iovec); // data region starts after the iovec header
        uint32_t payload = 0;
        for (uint32_t i = 0; i < n; i++) {
            uint32_t room = SENTRY_BUFSZ - cur;
            uint32_t want = (giov && giov[i].iov_len < room) ? (uint32_t)giov[i].iov_len : room;
            if (nr == 65 && want) {
                size_t prefix =
                    guest_accessible_prefix((uint64_t)(uintptr_t)giov[i].iov_base, want, HL_LOGICAL_VMA_WRITE);
                if (!prefix) {
                    if (!payload) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        atomic_store_explicit(&R->busy, 0, memory_order_release);
                        return;
                    }
                    n = i;
                    worker_iovn = n;
                    break;
                }
                want = (uint32_t)prefix;
            }
            if (nr == 66 && want && giov) {
                ssize_t copied = guest_copy_from(R->buf + cur, (uint64_t)(uintptr_t)giov[i].iov_base, want);
                if (copied != (ssize_t)want) {
                    if (copied <= 0 && payload == 0) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        atomic_store_explicit(&R->busy, 0, memory_order_release);
                        return;
                    }
                    if (copied <= 0) {
                        n = i;
                        worker_iovn = n;
                        break;
                    }
                    want = (uint32_t)copied;
                }
            }
            biov[i].iov_base = (void *)(uintptr_t)cur; // buf-relative offset; sentry rebases + checks
            biov[i].iov_len = want;
            cur += want;
            payload += want;
        }
        R->inlen = cur;
        R->redir[1] = 0;
        R->iovn = n;
        R->a[2] = n; // sentry runs the (possibly clamped) segment count
        break;
    }
    // ---- socket family ---- (sentry owns the real socket fd; only sockaddr/optval/data bytes cross,
    // never a guest pointer; all AF/port-map/jail translation runs inside service_local on the sentry)
    case 200:   // bind(fd, a1=addr, a2=addrlen)
    case 203: { // connect(fd, a1=addr, a2=addrlen): in-sockaddr -> tail window
        const uint8_t *sa = (const uint8_t *)G_A1(c);
        if (sa) {
            uint32_t n = (uint32_t)G_A2(c);
            if (n > SENTRY_SADDRCAP) n = SENTRY_SADDRCAP; // real sockaddrs are <=128; cap defensively
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_SADDR_OFF, G_A1(c), n);
            R->redir[1] = SENTRY_SADDR_OFF;
            R->a[2] = n;
            R->inlen = n;
        } // NULL addr: leave register/len as-is (service_local handles it / errors identically)
        break;
    }
    case 202:   // accept(fd, a1=addr_out, a2=addrlen_inout)
    case 242:   // accept4(fd, a1=addr_out, a2=addrlen_inout, a3=flags)
    case 204:   // getsockname(fd, a1=addr_out, a2=addrlen_inout)
    case 205: { // getpeername(fd, a1=addr_out, a2=addrlen_inout): out-sockaddr + in/out socklen
        if (G_A1(c)) R->redir[1] = SENTRY_SADDR_OFF; // out sockaddr -> tail window
        if (G_A2(c)) {                               // in/out socklen: ship the guest cap
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_SLEN_OFF, G_A2(c), sizeof(socklen_t));
            memcpy(&worker_socklen, R->buf + SENTRY_SLEN_OFF, sizeof worker_socklen);
            worker_socklen_valid = 1;
            if (G_A1(c)) {
                size_t need = worker_socklen < SENTRY_SADDRCAP ? worker_socklen : SENTRY_SADDRCAP;
                SENTRY_REQUIRE_WRITE(G_A1(c), need);
            }
            R->redir[2] = SENTRY_SLEN_OFF;
        }
        break;
    }
    case 206: { // sendto(fd, a1=buf, a2=len, a3=flags, a4=destaddr, a5=addrlen): in-data + in-destaddr
        uint32_t n = G_A2(c) > SENTRY_DATACAP ? SENTRY_DATACAP : (uint32_t)G_A2(c);
        if (n) {
            ssize_t copied = guest_copy_from(R->buf, G_A1(c), n);
            if (copied <= 0) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                atomic_store_explicit(&R->busy, 0, memory_order_release);
                return;
            }
            n = (uint32_t)copied;
        }
        R->redir[1] = 0;
        R->a[2] = n; // short send is legal -> guest loops
        R->inlen = n;
        if (G_A4(c)) { // optional dest addr (UDP) -> tail window
            uint32_t dl = (uint32_t)G_A5(c);
            if (dl > SENTRY_SADDRCAP) dl = SENTRY_SADDRCAP;
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_SADDR_OFF, G_A4(c), dl);
            R->redir[4] = SENTRY_SADDR_OFF;
            R->a[5] = dl;
        }
        break;
    }
    case 207: { // recvfrom(fd, a1=buf, a2=len, a3=flags, a4=srcaddr_out, a5=addrlen_inout)
        uint32_t n = G_A2(c) > SENTRY_DATACAP ? SENTRY_DATACAP : (uint32_t)G_A2(c);
        if (n) {
            size_t prefix = guest_accessible_prefix(G_A1(c), n, HL_LOGICAL_VMA_WRITE);
            if (!prefix) {
                G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                atomic_store_explicit(&R->busy, 0, memory_order_release);
                return;
            }
            n = (uint32_t)prefix;
        }
        R->redir[1] = 0;
        R->a[2] = n;                                 // short recv is legal -> guest loops
        if (G_A4(c)) R->redir[4] = SENTRY_SADDR_OFF; // out src sockaddr -> tail window
        if (G_A5(c)) {                               // in/out socklen: ship the guest cap
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_SLEN_OFF, G_A5(c), sizeof(socklen_t));
            memcpy(&worker_socklen, R->buf + SENTRY_SLEN_OFF, sizeof worker_socklen);
            worker_socklen_valid = 1;
            if (G_A4(c)) {
                size_t need = worker_socklen < SENTRY_SADDRCAP ? worker_socklen : SENTRY_SADDRCAP;
                SENTRY_REQUIRE_WRITE(G_A4(c), need);
            }
            R->redir[5] = SENTRY_SLEN_OFF;
        }
        break;
    }
    case 208: { // setsockopt(fd, a1=level, a2=optname, a3=optval, a4=optlen): in-optval -> opt window
        if (G_A3(c)) {
            uint32_t n = (uint32_t)G_A4(c);
            if (n > SENTRY_OPTCAP) n = SENTRY_OPTCAP;
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_OPT_OFF, G_A3(c), n);
            R->redir[3] = SENTRY_OPT_OFF;
            R->a[4] = n;
            R->inlen = n;
        }
        break;
    }
    case 209: {        // getsockopt(fd, a1=level, a2=optname, a3=optval_out, a4=optlen_inout)
        if (G_A4(c)) { // in/out optlen: ship the guest cap (clamped so the kernel can't overrun the window)
            socklen_t cap;
            SENTRY_IMPORT_EXACT(&cap, G_A4(c), sizeof cap);
            worker_socklen = cap;
            worker_socklen_valid = 1;
            if (G_A3(c)) {
                size_t need = cap < SENTRY_OPTCAP ? cap : SENTRY_OPTCAP;
                SENTRY_REQUIRE_WRITE(G_A3(c), need);
            }
            if (cap > SENTRY_OPTCAP) cap = SENTRY_OPTCAP;
            *(socklen_t *)(R->buf + SENTRY_SLEN_OFF) = cap;
            R->redir[4] = SENTRY_SLEN_OFF;
        }
        if (G_A3(c)) R->redir[3] = SENTRY_OPT_OFF; // out optval -> opt window
        break;
    }
    // ---- sendmsg/recvmsg (item 2): flatten the guest msghdr GRAPH into the ring ----
    case 211:   // sendmsg(fd, a1=msghdr, flags)
    case 212: { // recvmsg(fd, a1=msghdr, flags)
        if (!G_A1(c)) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return;
        }
        SENTRY_IMPORT_EXACT(worker_msghdr, G_A1(c), SENTRY_MSGHDR_SZ);
        worker_msghdr_valid = 1;
        uint64_t g_name = *(uint64_t *)(worker_msghdr + 0);
        uint32_t g_namelen = *(uint32_t *)(worker_msghdr + 8);
        uint64_t g_iov = *(uint64_t *)(worker_msghdr + 16);
        uint64_t g_iovlen = *(uint64_t *)(worker_msghdr + 24);
        uint64_t g_ctl = *(uint64_t *)(worker_msghdr + 32);
        uint64_t g_ctllen = *(uint64_t *)(worker_msghdr + 40);
        uint32_t g_flags = *(uint32_t *)(worker_msghdr + 48);
        if (g_nonpie_lo) {
            g_name = nonpie_p(g_name);
            g_iov = nonpie_p(g_iov);
            g_ctl = nonpie_p(g_ctl);
        }
        uint8_t *h = R->buf; // the 56-byte msghdr COPY at [0,56)
        memset(h, 0, SENTRY_MSGHDR_SZ);
        // msg_name: offset into the sockaddr tail window (send copies the addr; recv just reserves it).
        if (g_name && g_namelen) {
            uint32_t nl = g_namelen > SENTRY_SADDRCAP ? SENTRY_SADDRCAP : g_namelen;
            if (nr == 211) SENTRY_IMPORT_EXACT(R->buf + SENTRY_MSGNAME_OFF, g_name, nl);
            *(uint64_t *)(h + 0) = SENTRY_MSGNAME_OFF; // nonzero offset == present
            *(uint32_t *)(h + 8) = nl;                 // capped to the ring window (real addrs fit)
        }
        // msg_iov: iovec[] header (iov_base = OFFSET) + data, flattened like readv/writev, capped to DATACAP.
        uint32_t n = g_iovlen > SENTRY_IOVMAX ? SENTRY_IOVMAX : (uint32_t)g_iovlen;
        if (n && guest_iov_import(g_iov, n, worker_msg_iov) < 0) {
            G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
            atomic_store_explicit(&R->busy, 0, memory_order_release);
            return;
        }
        if (g_nonpie_lo)
            for (uint32_t i = 0; i < n; i++)
                worker_msg_iov[i].iov_base =
                    (void *)(uintptr_t)nonpie_p((uint64_t)(uintptr_t)worker_msg_iov[i].iov_base);
        worker_msg_iovn = n;
        const struct iovec *giov = worker_msg_iov;
        struct iovec *biov = (struct iovec *)(R->buf + SENTRY_MSGIOV_OFF);
        uint32_t cur = SENTRY_MSGIOV_OFF + n * (uint32_t)sizeof(struct iovec);
        uint32_t msg_payload = 0;
        for (uint32_t i = 0; i < n; i++) {
            uint32_t room = (cur < SENTRY_DATACAP) ? (SENTRY_DATACAP - cur) : 0; // keep data clear of the tail
            uint32_t want = (giov && giov[i].iov_len < room) ? (uint32_t)giov[i].iov_len : room;
            if (nr == 212 && want) {
                size_t prefix =
                    guest_accessible_prefix((uint64_t)(uintptr_t)giov[i].iov_base, want, HL_LOGICAL_VMA_WRITE);
                if (!prefix) {
                    if (!msg_payload) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        atomic_store_explicit(&R->busy, 0, memory_order_release);
                        return;
                    }
                    n = i;
                    worker_msg_iovn = n;
                    break;
                }
                want = (uint32_t)prefix;
            }
            if (nr == 211 && want) {
                ssize_t copied = guest_copy_from(R->buf + cur, (uint64_t)(uintptr_t)giov[i].iov_base, want);
                if (copied != (ssize_t)want) {
                    if (copied <= 0 && i == 0) {
                        G_RET(c) = (uint64_t)(int64_t)(-EFAULT);
                        atomic_store_explicit(&R->busy, 0, memory_order_release);
                        return;
                    }
                    if (copied <= 0) {
                        n = i;
                        worker_msg_iovn = n;
                        break;
                    }
                    want = (uint32_t)copied;
                }
            }
            biov[i].iov_base = (void *)(uintptr_t)cur;
            biov[i].iov_len = want;
            cur += want;
            msg_payload += want;
        }
        *(uint64_t *)(h + 16) = SENTRY_MSGIOV_OFF;
        *(uint64_t *)(h + 24) = n;
        // msg_control: offset into the optval tail window (send copies the cmsg; recv reserves it). SCM_RIGHTS
        // fds inside are sentry fds, so the bytes cross verbatim.
        if (g_ctl && g_ctllen) {
            uint32_t cl = g_ctllen > SENTRY_MSGCTLCAP ? SENTRY_MSGCTLCAP : (uint32_t)g_ctllen;
            if (nr == 211) SENTRY_IMPORT_EXACT(R->buf + SENTRY_MSGCTL_OFF, g_ctl, cl);
            *(uint64_t *)(h + 32) = SENTRY_MSGCTL_OFF; // nonzero offset == present
            *(uint64_t *)(h + 40) = cl;                // controllen (send: actual; recv: cap)
        }
        *(uint32_t *)(h + 48) = g_flags;
        R->redir[1] = 0; // a1 -> msghdr copy; the sentry rebases the inner offsets (snr 211/212)
        R->inlen = cur;
        break;
    }
    // ---- multiplexing over sentry-owned fds (item 3) ----
    case 73: { // ppoll(fds, nfds, timeout_ts, sigmask, sigsetsz)
        uint32_t nfds = (uint32_t)G_A1(c);
        uint32_t bytes = nfds * 8u; // sizeof(struct pollfd) == 8
        if (bytes > SENTRY_DATACAP) {
            bytes = SENTRY_DATACAP;
            nfds = bytes / 8u;
        }
        if (G_A0(c) && bytes) SENTRY_IMPORT_EXACT(R->buf, G_A0(c), bytes);
        R->redir[0] = 0;
        R->a[1] = nfds;
        if (G_A2(c)) {
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_POLL_TMO, G_A2(c), 16);
            R->redir[2] = SENTRY_POLL_TMO;
        } else
            R->a[2] = 0; // NULL timeout == block forever
        R->a[3] = 0;
        R->a[4] = 0; // sigmask ignored by service_local
        break;
    }
    case 72: { // pselect6(nfds, rd, wr, ex, timeout_ts, sigmask)
        uint32_t nfds = (uint32_t)G_A0(c);
        uint32_t fb = (nfds + 7u) / 8u;
        if (fb > 128u) fb = 128u; // fd_set caps at FD_SETSIZE/8 == 128
        if (G_A1(c)) {
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_PSEL_RD, G_A1(c), fb);
            R->redir[1] = SENTRY_PSEL_RD;
        } else
            R->a[1] = 0;
        if (G_A2(c)) {
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_PSEL_WR, G_A2(c), fb);
            R->redir[2] = SENTRY_PSEL_WR;
        } else
            R->a[2] = 0;
        if (G_A3(c)) {
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_PSEL_EX, G_A3(c), fb);
            R->redir[3] = SENTRY_PSEL_EX;
        } else
            R->a[3] = 0;
        if (G_A4(c)) {
            SENTRY_IMPORT_EXACT(R->buf + SENTRY_PSEL_TMO, G_A4(c), 16);
            R->redir[4] = SENTRY_PSEL_TMO;
        } else
            R->a[4] = 0;
        R->a[5] = 0;
        break;
    }
    case 21: // epoll_ctl(epfd, op, fd, a3=event): in epoll_event (SENTRY_EPEV_SZ, per guest arch)
        if (G_A3(c)) {
            SENTRY_IMPORT_EXACT(R->buf, G_A3(c), SENTRY_EPEV_SZ);
            R->redir[3] = 0;
        }
        break;
    case 22: // epoll_pwait(epfd, a1=events_out, maxevents, timeout, sigmask): reserve out window, drop sigmask
        if ((int64_t)G_A2(c) > 0) {
            uint64_t maxevents = G_A2(c);
            if (maxevents > SENTRY_BUFSZ / SENTRY_EPEV_SZ) maxevents = SENTRY_BUFSZ / SENTRY_EPEV_SZ;
            uint64_t bytes = maxevents * SENTRY_EPEV_SZ;
            SENTRY_REQUIRE_WRITE(G_A1(c), bytes);
            R->a[2] = maxevents;
        }
        R->redir[1] = 0;
        R->a[4] = 0;
        break;
    // ---- fd-table ops on sentry-owned fds (item 3) ----
    case 25: // fcntl(fd, cmd, arg): only F_GETLK/SETLK/SETLKW (5/6/7) carry a flock* in a2. Always redir a2
             // to the ring for those (so the sentry's flock deref can never hit a guest/NULL pointer); copy
             // the inbound lock only if the guest pointer is real.
        if ((int)G_A1(c) >= 5 && (int)G_A1(c) <= 7) {
            if (G_A2(c)) SENTRY_IMPORT_EXACT(R->buf, G_A2(c), SENTRY_FLOCKSZ);
            R->redir[2] = 0;
        }
        break;
    case 29: { // ioctl(fd, req, arg): always redir arg to the ring so the sentry never derefs a guest/NULL
               // pointer; copy in exactly the _IOC_SIZE/table byte count (unsized/unknown -> nothing -> ENOTTY)
        if (G_A2(c)) {
            uint32_t isz, osz;
            sentry_ioctl_sizes((unsigned long)G_A1(c), &isz, &osz);
            if (isz > SENTRY_IOCTLCAP) isz = SENTRY_IOCTLCAP;
            if (isz) SENTRY_IMPORT_EXACT(R->buf, G_A2(c), isz);
        }
        R->redir[2] = 0;
        break;
    }
    case 59: // pipe2(a0=int[2], flags): out fd pair
        SENTRY_REQUIRE_WRITE(G_A0(c), 8);
        R->redir[0] = 0;
        break;
    case 199: // socketpair(domain, type, proto, a3=int[2]): out fd pair
        SENTRY_REQUIRE_WRITE(G_A3(c), 8);
        R->redir[3] = 0;
        break;
    default:
        break; // 57 close / 62 lseek / 198 socket / 201 listen / 210 shutdown / 20 epoll_create1 /
               // 23 dup / 24 dup3: no buffer.
    }

    // ---- ring round-trip ----
    uint64_t request = atomic_fetch_add_explicit(&R->request, 1, memory_order_relaxed) + 1;
    atomic_store_explicit(&R->turn, 1, memory_order_release); // publish request -> sentry
    uint32_t spins = 0;
    while (atomic_load_explicit(&R->response, memory_order_acquire) != request) { // await this response
        if (++spins > 256) {
            sched_yield();
            spins = 0;
        }
    }

    // ---- copy outputs back into guest memory (guest pointers only ever touched here, on the worker) ----
    int64_t ret = R->ret;
    if (nr == 56 && ret >= 0 && ret < HL_NFD) {
        const char *opened_path = (const char *)R->buf; /* canonical path snapshot, never reread guest memory */
        if (!strcmp(opened_path, "/proc") || !strncmp(opened_path, "/proc/", 6) || !strcmp(opened_path, "/dev/fd"))
            snprintf(g_fdpath[(int)ret], sizeof g_fdpath[(int)ret], "%.*s", (int)sizeof(g_fdpath[(int)ret]) - 1,
                     opened_path);
    }
#define SENTRY_EXPORT_EXACT(dst, src, len)                                                                             \
    do {                                                                                                               \
        size_t _n = (size_t)(len);                                                                                     \
        if (_n && guest_copy_to((uint64_t)(dst), (src), _n) != (ssize_t)_n) ret = -EFAULT;                             \
    } while (0)
    switch (nr) {
    case 78: // readlinkat: sentry landed the non-NUL-terminated link bytes after the path window
        if (ret > 0) {
            uint32_t n = (uint32_t)ret;
            if (n > (uint32_t)R->a[3]) n = (uint32_t)R->a[3];
            ssize_t copied = guest_copy_to(G_A2(c), R->buf + SENTRY_PATHCAP, n);
            if (copied != (ssize_t)n) ret = copied > 0 ? copied : -EFAULT;
        }
        break;
    case 63: // read
    case 67: // pread64
    case 61: // getdents64: the sentry landed ret bytes at buf[0]
        if (ret > 0) {
            uint32_t n = (uint32_t)ret;
            if (n > (uint32_t)R->a[2]) n = (uint32_t)R->a[2]; // never exceed the window we shipped
            ssize_t copied = guest_copy_to(G_A1(c), R->buf, n);
            if (copied != (ssize_t)n) ret = copied > 0 ? copied : -EFAULT;
        }
        break;
    case 80: // fstat: struct landed at buf[0]
        if (ret == 0) SENTRY_EXPORT_EXACT(G_A1(c), R->buf, SENTRY_STATSZ);
        break;
    case 79: // newfstatat: struct landed at buf[SENTRY_PATHCAP]
        if (ret == 0) SENTRY_EXPORT_EXACT(G_A2(c), R->buf + SENTRY_PATHCAP, SENTRY_STATSZ);
        break;
    case 291: // statx: struct landed at buf[SENTRY_PATHCAP]
        if (ret == 0) SENTRY_EXPORT_EXACT(G_A4(c), R->buf + SENTRY_PATHCAP, SENTRY_STATXSZ);
        break;
    case 71:
        if (ret >= 0 && G_A2(c)) SENTRY_EXPORT_EXACT(G_A2(c), R->buf, sizeof(int64_t));
        break;
    case 76:  // splice: advanced in/out offsets land back in the guest's off_in/off_out
    case 285: // copy_file_range: same offset writeback shape
        if (ret >= 0) {
            if (G_A1(c)) SENTRY_EXPORT_EXACT(G_A1(c), R->buf, sizeof(int64_t));
            if (G_A3(c)) SENTRY_EXPORT_EXACT(G_A3(c), R->buf + sizeof(int64_t), sizeof(int64_t));
        }
        break;
    case 65: // readv: scatter the ret bytes the sentry fetched back into the guest iovecs
        if (ret > 0) {
            const struct iovec *giov = worker_iov;
            const struct iovec *biov = (const struct iovec *)R->buf;
            uint32_t n = worker_iovn, remaining = (uint32_t)ret;
            uint32_t delivered = 0;
            for (uint32_t i = 0; i < n && remaining; i++) {
                uint32_t seg = (uint32_t)biov[i].iov_len; // window length the sentry scattered into
                if (seg > remaining) seg = remaining;
                // the sentry rebased iov_base to a pointer into buf[] (shared at the same VA -> usable here)
                ssize_t copied = guest_copy_to((uint64_t)(uintptr_t)giov[i].iov_base, biov[i].iov_base, seg);
                if (copied != (ssize_t)seg) {
                    if (copied > 0) delivered += (uint32_t)copied;
                    ret = delivered ? (int64_t)delivered : -EFAULT;
                    break;
                }
                delivered += seg;
                remaining -= seg;
            }
        }
        break;
    // ---- socket family: scatter the out-sockaddr / its length / out-optval / recv data back ----
    case 202: // accept
    case 242: // accept4
    case 204: // getsockname
    case 205: // getpeername: sentry wrote the translated sockaddr to the tail window + the length to SLEN
        // accept/accept4 succeed with ret>=0 (the new fd); getsockname/getpeername with ret==0.
        if (ret >= 0 && G_A2(c)) {
            socklen_t outlen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF); // length service_local reported
            socklen_t gcap = worker_socklen_valid ? worker_socklen : 0;
            if (G_A1(c)) {
                socklen_t cpy = outlen < gcap ? outlen : gcap; // truncate to the guest buffer
                if (cpy > SENTRY_SADDRCAP) cpy = SENTRY_SADDRCAP;
                SENTRY_EXPORT_EXACT(G_A1(c), R->buf + SENTRY_SADDR_OFF, cpy);
            }
            if (ret >= 0) SENTRY_EXPORT_EXACT(G_A2(c), &outlen, sizeof outlen);
        }
        break;
    case 207: // recvfrom: recv data landed at buf[0]; src sockaddr + its length in the tail windows
        if (ret > 0) {
            uint32_t n = (uint32_t)ret;
            if (n > (uint32_t)R->a[2]) n = (uint32_t)R->a[2]; // never exceed the window we shipped
            ssize_t copied = guest_copy_to(G_A1(c), R->buf, n);
            if (copied != (ssize_t)n) ret = copied > 0 ? copied : -EFAULT;
        }
        if (ret >= 0 && G_A5(c)) {
            socklen_t outlen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF);
            socklen_t gcap = worker_socklen_valid ? worker_socklen : 0;
            if (G_A4(c)) {
                socklen_t cpy = outlen < gcap ? outlen : gcap;
                if (cpy > SENTRY_SADDRCAP) cpy = SENTRY_SADDRCAP;
                SENTRY_EXPORT_EXACT(G_A4(c), R->buf + SENTRY_SADDR_OFF, cpy);
            }
            if (ret >= 0) SENTRY_EXPORT_EXACT(G_A5(c), &outlen, sizeof outlen);
        }
        break;
    case 209: // getsockopt: optval landed at the opt window; its length at SLEN
        if (ret == 0 && G_A4(c)) {
            socklen_t outlen = *(socklen_t *)(R->buf + SENTRY_SLEN_OFF);
            socklen_t gcap = worker_socklen_valid ? worker_socklen : 0;
            socklen_t eff = gcap < SENTRY_OPTCAP ? gcap : SENTRY_OPTCAP; // we shipped at most OPTCAP
            if (G_A3(c)) {
                socklen_t cpy = outlen < eff ? outlen : eff;
                SENTRY_EXPORT_EXACT(G_A3(c), R->buf + SENTRY_OPT_OFF, cpy);
            }
            if (ret == 0) SENTRY_EXPORT_EXACT(G_A4(c), &outlen, sizeof outlen);
        }
        break;
    // ---- recvmsg (item 2): scatter received data + write back name/control/flags into the guest msghdr ----
    case 212:
        if (ret >= 0 && worker_msghdr_valid) {
            uint8_t *h = R->buf; // the ring msghdr copy service_local filled
            uint64_t g_name = *(uint64_t *)(worker_msghdr + 0);
            if (g_nonpie_lo) g_name = nonpie_p(g_name);
            uint32_t g_namecap = *(uint32_t *)(worker_msghdr + 8);
            uint32_t outnl = *(uint32_t *)(h + 8); // length the sentry reported
            if (g_name && g_namecap) {
                uint32_t cpy = outnl < g_namecap ? outnl : g_namecap;
                if (cpy > SENTRY_SADDRCAP) cpy = SENTRY_SADDRCAP;
                SENTRY_EXPORT_EXACT(g_name, R->buf + SENTRY_MSGNAME_OFF, cpy);
            }
            *(uint32_t *)(worker_msghdr + 8) = outnl;
            uint64_t g_ctl = *(uint64_t *)(worker_msghdr + 32);
            if (g_nonpie_lo) g_ctl = nonpie_p(g_ctl);
            uint64_t g_ctlcap = *(uint64_t *)(worker_msghdr + 40);
            uint64_t outcl = *(uint64_t *)(h + 40); // control length the sentry wrote
            if (g_ctl && g_ctlcap) {
                uint64_t cpy = outcl < g_ctlcap ? outcl : g_ctlcap;
                if (cpy > SENTRY_MSGCTLCAP) cpy = SENTRY_MSGCTLCAP;
                SENTRY_EXPORT_EXACT(g_ctl, R->buf + SENTRY_MSGCTL_OFF, cpy);
            }
            *(uint64_t *)(worker_msghdr + 40) = outcl;
            *(uint32_t *)(worker_msghdr + 48) = *(uint32_t *)(h + 48);
            // scatter the ret payload bytes back into the guest's iovec segments
            if (ret > 0) {
                const struct iovec *biov = (const struct iovec *)(R->buf + SENTRY_MSGIOV_OFF);
                uint32_t n = worker_msg_iovn;
                uint32_t remaining = (uint32_t)ret, delivered = 0;
                for (uint32_t i = 0; i < n && remaining; i++) {
                    uint32_t seg = (uint32_t)biov[i].iov_len;
                    if (seg > remaining) seg = remaining;
                    ssize_t copied =
                        guest_copy_to((uint64_t)(uintptr_t)worker_msg_iov[i].iov_base, biov[i].iov_base, seg);
                    if (copied != (ssize_t)seg) {
                        if (copied > 0) delivered += (uint32_t)copied;
                        ret = delivered ? (int64_t)delivered : -EFAULT;
                        break;
                    }
                    delivered += seg;
                    remaining -= seg;
                }
            }
            if (ret >= 0) SENTRY_EXPORT_EXACT(G_A1(c), worker_msghdr, SENTRY_MSGHDR_SZ);
        }
        break;
    // ---- multiplexing copy-back (item 3) ----
    case 73: // ppoll: copy back ONLY each entry's revents (+6, 2B). The sentry rewrote the ring pollfd.fd
             //   fields to REAL fds for the kernel, so the guest's own pollfd.fd/events must be left untouched.
        if (ret >= 0 && G_A0(c)) {
            uint32_t nf = (uint32_t)R->a[1];
            for (uint32_t k = 0; k < nf; k++)
                if (guest_copy_to(G_A0(c) + (size_t)k * 8u + 6u, R->buf + (size_t)k * 8u + 6u, 2u) != 2) {
                    ret = -EFAULT;
                    break;
                }
        }
        break;
    case 72: // pselect6: the three fd_sets were narrowed in place
        if (ret >= 0) {
            uint32_t fb = ((uint32_t)G_A0(c) + 7u) / 8u;
            if (fb > 128u) fb = 128u;
            if (G_A1(c)) SENTRY_EXPORT_EXACT(G_A1(c), R->buf + SENTRY_PSEL_RD, fb);
            if (G_A2(c)) SENTRY_EXPORT_EXACT(G_A2(c), R->buf + SENTRY_PSEL_WR, fb);
            if (G_A3(c)) SENTRY_EXPORT_EXACT(G_A3(c), R->buf + SENTRY_PSEL_EX, fb);
        }
        break;
    case 22: // epoll_pwait: ret ready events (SENTRY_EPEV_SZ each, per guest arch) landed at buf[0]
        if (ret > 0 && G_A1(c)) {
            uint32_t mx = (uint32_t)G_A2(c);
            uint32_t got = (uint32_t)ret < mx ? (uint32_t)ret : mx;
            SENTRY_EXPORT_EXACT(G_A1(c), R->buf, got * SENTRY_EPEV_SZ);
        }
        break;
    case 25: // fcntl F_GETLK: the conflicting lock was written back into the ring flock
        if ((int)G_A1(c) == 5 && ret >= 0 && G_A2(c)) SENTRY_EXPORT_EXACT(G_A2(c), R->buf, SENTRY_FLOCKSZ);
        break;
    case 29: // ioctl: write back exactly the out bytes the request defines (never clobber past them)
        if (ret >= 0 && G_A2(c)) {
            uint32_t isz, osz;
            sentry_ioctl_sizes((unsigned long)G_A1(c), &isz, &osz);
            if (osz > SENTRY_IOCTLCAP) osz = SENTRY_IOCTLCAP;
            if (osz) SENTRY_EXPORT_EXACT(G_A2(c), R->buf, osz);
        }
        break;
    case 59: // pipe2: out fd pair (both ends sentry fds, virtual to the guest)
        if (ret == 0 && G_A0(c)) SENTRY_EXPORT_EXACT(G_A0(c), R->buf, 8);
        break;
    case 199: // socketpair: out fd pair
        if (ret == 0 && G_A3(c)) SENTRY_EXPORT_EXACT(G_A3(c), R->buf, 8);
        break;
    default:
        break; // 56 openat / 57 close / 62 lseek / 64 write / 66 writev / 68 pwrite / 198 socket /
               // 200 bind / 203 connect / 206 sendto / 208 setsockopt / 210 shutdown / 211 sendmsg /
               // 20 epoll_create1 / 21 epoll_ctl / 23 dup / 24 dup3: no out bytes
    }
#undef SENTRY_EXPORT_EXACT
#undef SENTRY_REQUIRE_WRITE
#undef SENTRY_IMPORT_STRING
#undef SENTRY_IMPORT_EXACT
    G_RET(c) = (uint64_t)ret;
    atomic_store_explicit(&R->busy, 0, memory_order_release); // release the producer lock (round-trip done)
}

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
