#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif
#include "system.h"
#include "hl/engine.h"
#include "hl/linux_abi.h"

#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <sys/resource.h>
#if defined(__APPLE__)
#include <libproc.h>
#include <sys/sysctl.h>
#include <sys/syscall.h>
#endif
#include <unistd.h>

#define HL_PRIVATE_PROCESSES 1024u
#define HL_PRIVATE_CELLS 256u
#define HL_PRIVATE_FIXED_HEADROOM 64u
#define HL_PRIVATE_INIT 1u
#define HL_PRIVATE_LIVE 2u

typedef struct hl_private_process {
    _Atomic uint64_t claim;
    _Atomic uint64_t claim_start_ns;
    _Atomic uint32_t state;
    _Atomic int64_t pid;
    _Atomic uint64_t start_ns;
    _Atomic uint64_t generation;
    _Atomic uint64_t cells[HL_PRIVATE_CELLS]; /* high32=fd+1, low32=references */
} hl_private_process;

static hl_private_process *hl_private;
static _Atomic uint64_t *hl_private_epoch;
static uint64_t *hl_private_fork_cells;
static void hl_private_configure_limit(void);
static size_t hl_private_fork_count;
static pthread_mutex_t hl_private_fork_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_once_t hl_private_fork_atfork_once = PTHREAD_ONCE_INIT;
/* Identity of the thread that armed the managed fork protocol by taking the lock in
   hl_host_process_fd_private_fork_prepare().  Read only by the fork child handler. */
static _Atomic int hl_private_fork_armed;
static _Atomic(pthread_t) hl_private_fork_owner;

static void hl_private_fork_disarm(void) {
    atomic_store_explicit(&hl_private_fork_armed, 0, memory_order_relaxed);
}

/* pthread_atfork child handler.  fork() from a multi-threaded process copies
   hl_private_fork_lock in whatever state a sibling thread left it, and the child --
   which has only the calling thread -- then blocks on it forever.  This runs in the
   child before fork() returns, so it must not allocate, free, log, panic, or acquire
   a lock: every statement below is a plain store.

   The managed fork path (fork_prepare -> fork() -> fork_complete) deliberately hands
   the child a HELD lock and a heap snapshot that fork_complete() replays and unlocks,
   so that case must be left exactly as it is.  It is identified by the arming thread
   being the thread that called fork(); only that thread survives into the child.  Any
   other fork is unmanaged and gets the lock and the snapshot reset.

   The shared record table itself is NOT reset here.  It is a MAP_SHARED anonymous
   mapping, so the child sees the parent's live rows rather than a copy, and clearing
   it would destroy the parent's state.  It does not need resetting: rows are keyed by
   (pid, start_ns), the child's pid differs, so the child simply owns no rows and
   repopulates by claiming its own. */
static void hl_private_fork_child(void) {
    if (atomic_load_explicit(&hl_private_fork_armed, memory_order_relaxed) &&
        pthread_equal(atomic_load_explicit(&hl_private_fork_owner, memory_order_relaxed), pthread_self()))
        return;
    /* The parent may have been mid-snapshot inside fork_prepare().  Drop the inherited
       pointer without free(): free() takes the allocator lock and is not fork-safe here.
       Leaking one bounded buffer in the child is preferable to replaying a partial
       parent snapshot into the child's rows, which would be silent. */
    hl_private_fork_cells = NULL;
    hl_private_fork_count = 0;
    hl_private_fork_disarm();
    hl_private_fork_lock = (pthread_mutex_t)PTHREAD_MUTEX_INITIALIZER;
}

static void hl_private_register_atfork(void) {
    (void)pthread_atfork(NULL, NULL, hl_private_fork_child);
}

static uint64_t hl_private_process_start(int64_t pid) {
    uint64_t start_time_ns = 0;
    return hl_host_process_start_time_ns(pid, &start_time_ns) ? start_time_ns : 0;
}

static uint64_t hl_private_identity(int64_t pid, uint64_t start) {
    return ((uint64_t)(uint32_t)pid << 32) | ((uint32_t)start ^ (uint32_t)(start >> 32));
}

static void hl_private_cells_clear(hl_private_process *process) {
    for (unsigned index = 0; index < HL_PRIVATE_CELLS; ++index)
        atomic_store_explicit(&process->cells[index], 0, memory_order_relaxed);
}

static hl_private_process *hl_private_claim(int64_t pid, uint64_t start) {
    for (unsigned index = 0; index < HL_PRIVATE_PROCESSES; ++index) {
        hl_private_process *process = &hl_private[index];
        uint32_t state = atomic_load_explicit(&process->state, memory_order_acquire);
        if (state == HL_PRIVATE_INIT) {
            uint64_t claim = atomic_load_explicit(&process->claim, memory_order_acquire);
            int64_t claim_pid = (int64_t)(uint32_t)(claim >> 32);
            uint64_t live = hl_private_process_start(claim_pid);
            uint64_t claim_start = atomic_load_explicit(&process->claim_start_ns, memory_order_acquire);
            if (claim != 0 && live != 0 && live == claim_start) continue;
            atomic_store_explicit(&process->claim_start_ns, 0, memory_order_relaxed);
            uint64_t stale = claim;
            if (!atomic_compare_exchange_strong_explicit(&process->claim, &stale, 0, memory_order_acq_rel,
                                                         memory_order_relaxed))
                continue;
            atomic_store_explicit(&process->state, 0, memory_order_release);
            state = 0;
        }
        if (state == 0) {
            uint64_t claim = atomic_load_explicit(&process->claim, memory_order_acquire);
            if (claim != 0) {
                int64_t claim_pid = (int64_t)(uint32_t)(claim >> 32);
                uint64_t live = hl_private_process_start(claim_pid);
                if (live != 0 && hl_private_identity(claim_pid, live) == claim) continue;
                uint64_t stale = claim;
                if (!atomic_compare_exchange_strong_explicit(&process->claim, &stale, 0, memory_order_acq_rel,
                                                             memory_order_relaxed))
                    continue;
            }
        }
        if (state == HL_PRIVATE_LIVE) {
            int64_t record_pid = atomic_load_explicit(&process->pid, memory_order_relaxed);
            uint64_t record_start = atomic_load_explicit(&process->start_ns, memory_order_relaxed);
            uint64_t live_start = hl_private_process_start(record_pid);
            if (live_start != 0 && live_start == record_start) continue;
            uint32_t expected = HL_PRIVATE_LIVE;
            if (!atomic_compare_exchange_strong_explicit(&process->state, &expected, HL_PRIVATE_INIT,
                                                         memory_order_acq_rel, memory_order_relaxed))
                continue;
            hl_private_cells_clear(process);
            atomic_store_explicit(&process->claim_start_ns, 0, memory_order_relaxed);
            atomic_store_explicit(&process->claim, 0, memory_order_relaxed);
            atomic_store_explicit(&process->state, 0, memory_order_release);
            state = 0;
        }
        if (state != 0) continue;
        uint64_t empty_claim = 0;
        uint64_t mine = hl_private_identity(pid, start);
        if (!atomic_compare_exchange_strong_explicit(&process->claim, &empty_claim, mine, memory_order_acq_rel,
                                                     memory_order_relaxed))
            continue;
        atomic_store_explicit(&process->claim_start_ns, start, memory_order_relaxed);
        atomic_store_explicit(&process->state, HL_PRIVATE_INIT, memory_order_release);
        atomic_store_explicit(&process->pid, pid, memory_order_relaxed);
        atomic_store_explicit(&process->start_ns, start, memory_order_relaxed);
        hl_private_cells_clear(process);
        atomic_store_explicit(&process->state, HL_PRIVATE_LIVE, memory_order_release);
        return process;
    }
    return NULL;
}

static uint64_t hl_private_cell(int fd, uint32_t references) {
    return ((uint64_t)(uint32_t)(fd + 1) << 32) | references;
}

static void hl_private_cleanup(void) {
    int64_t pid = 0;
    uint64_t start = 0;
    (void)hl_host_process_self_identity(&pid, &start);
    for (unsigned index = 0; index < HL_PRIVATE_PROCESSES; ++index) {
        hl_private_process *process = &hl_private[index];
        if (atomic_load_explicit(&process->state, memory_order_acquire) != HL_PRIVATE_LIVE ||
            atomic_load_explicit(&process->pid, memory_order_relaxed) != pid ||
            atomic_load_explicit(&process->start_ns, memory_order_relaxed) != start)
            continue;
        uint32_t expected = HL_PRIVATE_LIVE;
        if (!atomic_compare_exchange_strong_explicit(&process->state, &expected, HL_PRIVATE_INIT, memory_order_acq_rel,
                                                     memory_order_relaxed))
            continue;
        hl_private_cells_clear(process);
        atomic_store_explicit(&process->claim_start_ns, 0, memory_order_relaxed);
        atomic_store_explicit(&process->claim, 0, memory_order_relaxed);
        atomic_store_explicit(&process->state, 0, memory_order_release);
    }
}

void hl_host_private_init(void) {
    size_t records_size = sizeof(*hl_private) * HL_PRIVATE_PROCESSES;
    (void)pthread_once(&hl_private_fork_atfork_once, hl_private_register_atfork);
    if (hl_private != NULL) return;
    hl_private_configure_limit();
    void *memory =
        mmap(NULL, records_size + sizeof(*hl_private_epoch), PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (memory != MAP_FAILED) {
        hl_private = memory;
        hl_private_epoch = (void *)((unsigned char *)memory + records_size);
        (void)atexit(hl_private_cleanup);
    }
}

#ifndef HL_EMBEDDED_BUILD
__attribute__((constructor)) static void hl_private_constructor(void) {
    hl_host_private_init();
}
#endif

static int hl_private_add_unlocked(int fd) {
    int64_t pid = 0;
    uint64_t start = 0;
    (void)hl_host_process_self_identity(&pid, &start);
    if (!hl_private || fd < 0 || fd == INT32_MAX || start == 0) return -ENOSPC;
    for (unsigned record = 0; record < HL_PRIVATE_PROCESSES; ++record) {
        hl_private_process *process = &hl_private[record];
        if (atomic_load_explicit(&process->state, memory_order_acquire) != HL_PRIVATE_LIVE ||
            atomic_load_explicit(&process->pid, memory_order_relaxed) != pid ||
            atomic_load_explicit(&process->start_ns, memory_order_relaxed) != start)
            continue;
        for (unsigned index = 0; index < HL_PRIVATE_CELLS; ++index) {
            uint64_t value = atomic_load_explicit(&process->cells[index], memory_order_acquire);
            if ((uint32_t)(value >> 32) == (uint32_t)(fd + 1)) {
                for (;;) {
                    uint32_t references = (uint32_t)value;
                    if (references == UINT32_MAX) return -EOVERFLOW;
                    uint64_t next = hl_private_cell(fd, references + 1);
                    if (atomic_compare_exchange_weak_explicit(&process->cells[index], &value, next,
                                                              memory_order_acq_rel, memory_order_relaxed)) {
                        atomic_fetch_add_explicit(&process->generation, 1, memory_order_release);
                        atomic_fetch_add_explicit(hl_private_epoch, 1, memory_order_release);
                        return 0;
                    }
                }
            }
            if (value == 0) {
                uint64_t empty = 0;
                if (atomic_compare_exchange_strong_explicit(&process->cells[index], &empty, hl_private_cell(fd, 1),
                                                            memory_order_acq_rel, memory_order_relaxed)) {
                    atomic_fetch_add_explicit(&process->generation, 1, memory_order_release);
                    atomic_fetch_add_explicit(hl_private_epoch, 1, memory_order_release);
                    return 0;
                }
            }
        }
    }
    hl_private_process *process = hl_private_claim(pid, start);
    if (!process) return -ENOSPC;
    atomic_store_explicit(&process->cells[0], hl_private_cell(fd, 1), memory_order_release);
    atomic_fetch_add_explicit(&process->generation, 1, memory_order_release);
    atomic_fetch_add_explicit(hl_private_epoch, 1, memory_order_release);
    return 0;
}

int hl_host_process_fd_private_add(int fd) {
    int result;
    if (pthread_mutex_lock(&hl_private_fork_lock) != 0) return -EDEADLK;
    result = hl_private_add_unlocked(fd);
    (void)pthread_mutex_unlock(&hl_private_fork_lock);
    return result;
}

/* The private-descriptor floor is derived from the host RLIMIT_NOFILE soft limit (and, on Darwin, from
 * kern.maxfilesperproc). hl_host_process_fd_private_adopt() calls it once per adopted descriptor, which
 * made getrlimit 15 of the 20 the engine issued per guest open() on Linux and 20 of 20 on macOS.
 *
 * Guest limit changes are emulated end to end in g_limits (linux_abi/container/state.c), and that layer
 * cannot name the host call. hl_private_configure_limit() is the deliberate host exception: on Linux it
 * may raise RLIMIT_NOFILE for the whole engine process, and fork/native-exec children inherit that larger
 * host table. The guest ceiling is captured first and remains the boundary, so neither the guest nor a
 * low descriptor it names can enter the enlarged private band. The floor cache is still keyed on process
 * identity, so a child re-derives once and never inherits a floor stamped by a different process.
 *
 * What that reasoning does NOT cover is an EXTERNAL prlimit(2) aimed at this process, which is the one
 * way the soft limit can drop under us. Only a DROP is harmful -- a raised limit merely leaves the
 * private band lower than it could be, and F_DUPFD_CLOEXEC to a lower floor still succeeds. So adopt
 * treats a failed relocation as a reason to discard the cache and re-derive once, which restores the
 * per-descriptor observation exactly where its absence could change an answer, and nowhere else. */
static _Atomic int hl_private_floor_value;
static _Atomic int64_t hl_private_floor_pid;
static _Atomic uint64_t hl_private_floor_start;
static _Atomic uint32_t hl_private_guest_limit;

static void hl_private_floor_forget(void) {
    atomic_store_explicit(&hl_private_floor_pid, 0, memory_order_release);
}

static rlim_t hl_private_guest_ceiling(rlim_t ceiling) {
    if (ceiling == RLIM_INFINITY || ceiling > INT32_MAX) ceiling = INT32_MAX;
    /* Keep one sixteenth of a small descriptor table private, up to the 4096
     * slots used on a large host.  A fixed 4096-slot reserve made an engine
     * started from an ordinary `ulimit -n 1024` shell refuse before loading a
     * guest at all.  The proportional reserve leaves 64 private descriptors in
     * that case while preserving the existing 65536 guest ceiling on the
     * generous limits used by containers and CI. */
    rlim_t reserve = ceiling / 16u;
    if (reserve > HL_HOST_PRIVATE_DESCRIPTOR_MINIMUM) reserve = HL_HOST_PRIVATE_DESCRIPTOR_MINIMUM;
    if (reserve < 16u) reserve = 16u;
    if (ceiling <= reserve + 3u) return 0;
    rlim_t guest = ceiling - reserve;
    if (guest > HL_LINUX_FD_LIMIT) guest = HL_LINUX_FD_LIMIT;
    return guest;
}

static void hl_private_configure_limit(void) {
#if !defined(__APPLE__)
    struct rlimit limit;
    if (getrlimit(RLIMIT_NOFILE, &limit) != 0) return;
    rlim_t guest = hl_private_guest_ceiling(limit.rlim_cur);
    if (guest == 0) return;
    /* A mapped guest file can retain both its typed host handle and a mapping backing. Keeping one sixteenth of an
     * ordinary 1024-entry table private therefore exhausts the host band after 32 simultaneously-open files,
     * even though the guest was told it could open 960. When the inherited hard limit permits it, widen only
     * the engine's host table and retain the inherited guest ceiling below. No privilege is required to raise
     * a soft limit to its existing hard limit, and the 4096-slot band covers two backing descriptors for every
     * guest-visible slot plus process-wide engine handles. The raise is intentionally process-wide: every
     * sibling shares this private namespace, while the emulated guest limit remains separately captured. */
    /* A host-backed engine can itself run as a guest of another engine. In that
     * shape every descriptor it places in its private band is represented by a
     * descriptor in the outer engine's private band too. Raising only enough
     * for this layer (`guest + reserve`) leaves the outer relocation floor above
     * the physical soft limit. When the inherited hard limit permits it, make
     * the complete guest interval plus one private band physically addressable;
     * the guest-visible ceiling below remains the pre-raise value. */
    rlim_t desired = (rlim_t)HL_LINUX_FD_LIMIT + HL_HOST_PRIVATE_DESCRIPTOR_MINIMUM;
    if (limit.rlim_cur < desired && (limit.rlim_max == RLIM_INFINITY || limit.rlim_max >= desired)) {
        rlim_t inherited = limit.rlim_cur;
        limit.rlim_cur = desired;
        if (setrlimit(RLIMIT_NOFILE, &limit) != 0) limit.rlim_cur = inherited;
    }
    if (limit.rlim_cur < desired) {
        /* With an equally low hard limit there is nowhere to create a larger host-only band. A regular
         * guest file can consume both its typed handle and a retained mapping backing, so advertise no more
         * than one third of the real table rather than promising descriptors the bridge cannot represent. */
        rlim_t enforceable =
            limit.rlim_cur > HL_PRIVATE_FIXED_HEADROOM ? (limit.rlim_cur - HL_PRIVATE_FIXED_HEADROOM) / 3u : 0;
        if (guest > enforceable) guest = enforceable;
    }
    atomic_store_explicit(&hl_private_guest_limit, (uint32_t)guest, memory_order_release);
#endif
}

static int hl_private_floor_derive(void) {
    struct rlimit limit;
    if (getrlimit(RLIMIT_NOFILE, &limit) != 0) return -errno;
#if defined(__APPLE__)
    /* macOS enforces kern.maxfilesperproc (commonly 10240-24576) as the REAL per-process descriptor ceiling,
     * independent of -- and often far below -- RLIMIT_NOFILE's soft limit (a fresh macos-26 runner reports a
     * 1048576 soft limit, capped here to HL_LINUX_FD_LIMIT=65536). The private-fd floor would then sit above
     * that real ceiling; F_DUPFD_CLOEXEC to it fails EINVAL, so hl_host_process_fd_private_adopt failed for
     * every host descriptor and open_relative("/") returned HL_STATUS_RESOURCE_LIMIT on the runner (a dev
     * host whose maxfilesperproc >= the floor passed). Anchor the private interval just under the real kernel
     * ceiling: floor = maxfilesperproc - reserve, keeping `reserve` slots that F_DUPFD will accept. This can
     * be below the generous HL_HOST_GUEST_DESCRIPTOR_MINIMUM, which is fine -- it is the true ceiling. */
    {
        int maxfiles = 0;
        size_t maxfiles_size = sizeof(maxfiles);
        if (sysctlbyname("kern.maxfilesperproc", &maxfiles, &maxfiles_size, NULL, 0) == 0 && maxfiles > 0 &&
            (rlim_t)maxfiles < limit.rlim_cur)
            limit.rlim_cur = (rlim_t)maxfiles;
        rlim_t mac_guest = hl_private_guest_ceiling(limit.rlim_cur);
        return mac_guest != 0 ? (int)mac_guest : -EMFILE;
    }
#endif
    /* Split the native descriptor namespace into a low guest interval and a high private interval.  The
     * old `host_limit - 4096` floor accidentally capped all typed host handles at 4096 even when the host
     * offered hundreds of thousands of descriptors; MySQL's table cache can legitimately cross that
     * boundary. Begin the private interval immediately after the enforceable guest ceiling, with the
     * proportional reserve above keeping small host tables usable too. */
    /* Anchor the private interval just under the real ceiling rather than refusing when the soft limit does
     * not clear HL_HOST_GUEST_DESCRIPTOR_MINIMUM: below that minimum it is not the true ceiling (same as the
     * Darwin branch above).  Refusing broke engine-in-engine, where the inner guest reports RLIMIT_NOFILE
     * 20480 < MINIMUM + reserve. hl_private_configure_limit() can raise the host soft limit, but preserves
     * the inherited guest ceiling separately so the guest-visible answer stays stable. */
    rlim_t guest = atomic_load_explicit(&hl_private_guest_limit, memory_order_acquire);
    if (guest == 0 || guest >= limit.rlim_cur) guest = hl_private_guest_ceiling(limit.rlim_cur);
    return guest != 0 ? (int)guest : -EMFILE;
}

int hl_host_process_fd_private_floor(void) {
    int64_t pid = 0;
    uint64_t start = 0;
    int floor;
    if (!hl_host_process_self_identity(&pid, &start) || pid <= 0) return hl_private_floor_derive();
    if (atomic_load_explicit(&hl_private_floor_pid, memory_order_acquire) == pid &&
        atomic_load_explicit(&hl_private_floor_start, memory_order_acquire) == start)
        return atomic_load_explicit(&hl_private_floor_value, memory_order_acquire);
    floor = hl_private_floor_derive();
    if (floor < 0) return floor;
    atomic_store_explicit(&hl_private_floor_value, floor, memory_order_release);
    atomic_store_explicit(&hl_private_floor_start, start, memory_order_release);
    atomic_store_explicit(&hl_private_floor_pid, pid, memory_order_release);
    return floor;
}

uint32_t hl_engine_guest_fd_limit(void) {
    // The guest-visible fd ceiling (RLIMIT_NOFILE, /proc/self/limits) is HL_LINUX_FD_LIMIT-capped and derived
    // from the host RLIMIT_NOFILE. Generous hosts therefore keep the golden-stable 65536 answer, while a host
    // whose real table is smaller reports the lower enforceable boundary rather than refusing to start.
    // It deliberately does NOT apply the macOS kern.maxfilesperproc clamp that hl_host_process_fd_private_floor
    // uses: that clamp only bounds where the engine hoists its OWN host descriptors (F_DUPFD target), a
    // host-side concern invisible to the guest. Decoupling keeps getrlimit/proc consistent with the Linux
    // engine (65536) on a macos runner whose real per-process fd ceiling is lower, while adopt still lands
    // its private fds under that ceiling. (Guest fd numbers stay low in practice, far below the private band.)
    struct rlimit limit;
    if (getrlimit(RLIMIT_NOFILE, &limit) != 0) return 0;
    uint32_t configured = atomic_load_explicit(&hl_private_guest_limit, memory_order_acquire);
    if (configured != 0 && configured < limit.rlim_cur) return configured;
    return (uint32_t)hl_private_guest_ceiling(limit.rlim_cur);
}

int hl_host_process_fd_private_adopt(int fd) {
    if (fd < 0) return -EBADF;
    int floor = hl_host_process_fd_private_floor();
    if (floor < 0) return floor;
    int relocated = fd >= floor ? fd : fcntl(fd, F_DUPFD_CLOEXEC, floor);
    if (relocated < 0) {
        /* The floor is cached, so a relocation failure is the one moment worth paying a fresh getrlimit
         * for: an external prlimit(2) that lowered the soft limit under us leaves the cached floor above
         * the new ceiling and every adopt would fail forever otherwise. Re-derive and retry exactly once;
         * a floor that did not move means the failure was the descriptor's, not the limit's. */
        int saved = errno;
        hl_private_floor_forget();
        int refreshed = hl_host_process_fd_private_floor();
        if (refreshed < 0 || refreshed == floor) {
            errno = saved;
            return -saved;
        }
        floor = refreshed;
        relocated = fd >= floor ? fd : fcntl(fd, F_DUPFD_CLOEXEC, floor);
        if (relocated < 0) return -errno;
    }
    int status = hl_host_process_fd_private_add(relocated);
    if (status != 0) {
        if (relocated != fd) close(relocated);
        return status;
    }
    if (relocated != fd) close(fd);
    return relocated;
}

typedef struct hl_host_process_fd_private_relocation {
    int source;
    int private_descriptor;
} hl_host_process_fd_private_relocation;

struct hl_host_process_fd_private_plan {
    int minimum;
    int floor;
    size_t capacity;
    size_t scratch_size;
    size_t count;
    hl_host_process_fd_private_relocation relocations[];
};

#if defined(__APPLE__)
static void *hl_private_plan_scratch(const hl_host_process_fd_private_plan *plan) {
    return (void *)(plan->relocations + plan->capacity);
}

static int hl_private_plan_open_descriptors(void *buffer, int size) {
    /* proc_pidinfo is a libproc wrapper and is not promised async-signal-safe. This path runs after fork in
     * a multithreaded embedder, so enter XNU's documented proc_info syscall directly: call 2 is
     * PROC_INFO_CALL_PIDINFO and the remaining arguments exactly match proc_pidinfo(pid, flavor, arg,...). */
    long result = syscall(SYS_proc_info, 2, (int)getpid(), PROC_PIDLISTFDS, 0, buffer, size);
    if (result < 0) return -1;
    if (result > INT32_MAX) {
        errno = EOVERFLOW;
        return -1;
    }
    return (int)result;
}
#endif

int hl_host_process_fd_private_plan_release(hl_host_process_fd_private_plan **plan) {
    int result = 0;
    if (plan == NULL) return -EINVAL;
    if (*plan == NULL) return 0;
    for (size_t index = 0; index < (*plan)->count; ++index) {
        int descriptor = (*plan)->relocations[index].private_descriptor;
        /* POSIX leaves descriptor state ambiguous after EINTR, so retrying could close a number another
         * thread has already reused. Close exactly once, finish the whole batch, and report the first error. */
        if (close(descriptor) != 0 && result == 0) result = -errno;
    }
    free(*plan);
    *plan = NULL;
    return result;
}

int hl_host_process_fd_private_plan_prepare(int minimum, const int *descriptors, size_t descriptor_count,
                                            hl_host_process_fd_private_plan **out) {
    hl_host_process_fd_private_plan *plan = NULL;
    int floor;
    int result = 0;
    size_t scratch_size = 0;
    if (out == NULL || minimum < 0 || (descriptor_count != 0 && descriptors == NULL)) return -EINVAL;
    *out = NULL;
    floor = hl_host_process_fd_private_floor();
    if (floor < 0) return floor;
#if defined(__APPLE__)
    if ((size_t)floor + HL_HOST_PRIVATE_DESCRIPTOR_MINIMUM > SIZE_MAX / sizeof(struct proc_fdinfo)) return -EOVERFLOW;
    scratch_size = ((size_t)floor + HL_HOST_PRIVATE_DESCRIPTOR_MINIMUM) * sizeof(struct proc_fdinfo);
#endif
    if (descriptor_count > (SIZE_MAX - sizeof(*plan)) / sizeof(*plan->relocations)) return -EOVERFLOW;
    size_t records_size = descriptor_count * sizeof(*plan->relocations);
    if (scratch_size > SIZE_MAX - sizeof(*plan) - records_size) return -EOVERFLOW;
    plan = calloc(1, sizeof(*plan) + records_size + scratch_size);
    if (plan == NULL) return -ENOMEM;
    plan->minimum = minimum;
    plan->floor = floor;
    plan->capacity = descriptor_count;
    plan->scratch_size = scratch_size;
    for (size_t index = 0; index < descriptor_count; ++index) {
        int descriptor = descriptors[index];
        if (descriptor < 0) continue;
        if (fcntl(descriptor, F_GETFD) < 0) {
            result = -errno;
            goto done;
        }
        int duplicate = fcntl(descriptor, F_DUPFD_CLOEXEC, floor);
        if (duplicate < 0) {
            result = -errno;
            goto done;
        }
        plan->relocations[plan->count++] = (hl_host_process_fd_private_relocation){descriptor, duplicate};
    }
    result = 0;
    *out = plan;
    plan = NULL;
done:
    (void)hl_host_process_fd_private_plan_release(&plan);
    return result;
}

int hl_host_process_fd_private_plan_descriptor(const hl_host_process_fd_private_plan *plan, int descriptor) {
    if (descriptor < 0 || plan == NULL) return descriptor;
    for (size_t index = 0; index < plan->count; ++index)
        if (plan->relocations[index].source == descriptor) return plan->relocations[index].private_descriptor;
    if (descriptor < plan->minimum || descriptor >= plan->floor) return descriptor;
    return -1;
}

int hl_host_process_fd_private_plan_child(const hl_host_process_fd_private_plan *plan) {
    if (plan == NULL) return 0;
#if defined(__linux__)
    if (plan->minimum < plan->floor && close_range((unsigned)plan->minimum, (unsigned)(plan->floor - 1), 0) != 0)
        return -errno;
#else
    int needed = hl_private_plan_open_descriptors(NULL, 0);
    if (needed <= 0) return errno == 0 ? -EIO : -errno;
    if ((size_t)needed > plan->scratch_size) return -EOVERFLOW;
    int received = hl_private_plan_open_descriptors(hl_private_plan_scratch(plan), needed);
    if (received <= 0) return errno == 0 ? -EIO : -errno;
    if (received > needed || received % (int)sizeof(struct proc_fdinfo) != 0) return -EIO;
    struct proc_fdinfo *entries = hl_private_plan_scratch(plan);
    size_t count = (size_t)received / sizeof(*entries);
    for (size_t index = 0; index < count; ++index) {
        int descriptor = entries[index].proc_fd;
        if (descriptor < plan->minimum || descriptor >= plan->floor) continue;
        if (close(descriptor) != 0 && errno != EBADF) return -errno;
    }
#endif
    /* An explicitly retained endpoint can occupy 0..2 when the embedder closed a standard descriptor, or
     * can already be above the private floor. Its high duplicate is the child authority; retire the source
     * that the interval operation intentionally did not cover. */
    for (size_t index = 0; index < plan->count; ++index) {
        int source = plan->relocations[index].source;
        if (source >= plan->minimum && source < plan->floor) continue;
        if (close(source) != 0 && errno != EBADF) return -errno;
    }
    return 0;
}

static void hl_private_remove_unlocked(int fd) {
    int64_t pid = 0;
    uint64_t start = 0;
    (void)hl_host_process_self_identity(&pid, &start);
    if (!hl_private || fd < 0) return;
    for (unsigned record = 0; record < HL_PRIVATE_PROCESSES; ++record) {
        hl_private_process *process = &hl_private[record];
        if (atomic_load_explicit(&process->state, memory_order_acquire) != HL_PRIVATE_LIVE ||
            atomic_load_explicit(&process->pid, memory_order_relaxed) != pid ||
            atomic_load_explicit(&process->start_ns, memory_order_relaxed) != start)
            continue;
        for (unsigned index = 0; index < HL_PRIVATE_CELLS; ++index) {
            uint64_t value = atomic_load_explicit(&process->cells[index], memory_order_acquire);
            if ((uint32_t)(value >> 32) != (uint32_t)(fd + 1)) continue;
            for (;;) {
                uint32_t references = (uint32_t)value;
                uint64_t next = references > 1 ? hl_private_cell(fd, references - 1) : 0;
                if (atomic_compare_exchange_weak_explicit(&process->cells[index], &value, next, memory_order_acq_rel,
                                                          memory_order_relaxed)) {
                    atomic_fetch_add_explicit(&process->generation, 1, memory_order_release);
                    atomic_fetch_add_explicit(hl_private_epoch, 1, memory_order_release);
                    return;
                }
                if ((uint32_t)(value >> 32) != (uint32_t)(fd + 1)) return;
            }
        }
    }
}

void hl_host_process_fd_private_remove(int fd) {
    if (pthread_mutex_lock(&hl_private_fork_lock) != 0) return;
    hl_private_remove_unlocked(fd);
    (void)pthread_mutex_unlock(&hl_private_fork_lock);
}

int hl_host_process_fd_private_is(int64_t pid, uint64_t start_ns, int fd) {
    if (!hl_private || fd < 0) return 0;
    for (unsigned record = 0; record < HL_PRIVATE_PROCESSES; ++record) {
        hl_private_process *process = &hl_private[record];
        if (atomic_load_explicit(&process->state, memory_order_acquire) != HL_PRIVATE_LIVE ||
            atomic_load_explicit(&process->pid, memory_order_relaxed) != pid ||
            atomic_load_explicit(&process->start_ns, memory_order_relaxed) != start_ns)
            continue;
        for (unsigned index = 0; index < HL_PRIVATE_CELLS; ++index) {
            uint64_t value = atomic_load_explicit(&process->cells[index], memory_order_acquire);
            if ((uint32_t)(value >> 32) == (uint32_t)(fd + 1) && (uint32_t)value != 0) {
                if (atomic_load_explicit(&process->state, memory_order_acquire) == HL_PRIVATE_LIVE &&
                    atomic_load_explicit(&process->pid, memory_order_relaxed) == pid &&
                    atomic_load_explicit(&process->start_ns, memory_order_relaxed) == start_ns)
                    return 1;
                break;
            }
        }
    }
    return 0;
}

int hl_host_process_fd_private_current(int fd) {
    int64_t pid = 0;
    uint64_t start = 0;
    (void)hl_host_process_self_identity(&pid, &start);
    return hl_host_process_fd_private_is(pid, start, fd);
}

size_t hl_host_process_fd_private_count_current(void) {
    int64_t pid = 0;
    uint64_t start = 0;
    (void)hl_host_process_self_identity(&pid, &start);
    size_t count = 0;
    if (!hl_private) return 0;
    for (unsigned record = 0; record < HL_PRIVATE_PROCESSES; ++record) {
        hl_private_process *process = &hl_private[record];
        if (atomic_load_explicit(&process->state, memory_order_acquire) != HL_PRIVATE_LIVE ||
            atomic_load_explicit(&process->pid, memory_order_relaxed) != pid ||
            atomic_load_explicit(&process->start_ns, memory_order_relaxed) != start)
            continue;
        for (unsigned index = 0; index < HL_PRIVATE_CELLS; ++index)
            count += (uint32_t)atomic_load_explicit(&process->cells[index], memory_order_acquire);
    }
    return count;
}

int hl_host_process_fd_private_fork_prepare(void) {
    int64_t pid = 0;
    uint64_t start = 0;
    (void)hl_host_process_self_identity(&pid, &start);
    if (pthread_mutex_lock(&hl_private_fork_lock) != 0) return -EDEADLK;
    atomic_store_explicit(&hl_private_fork_owner, pthread_self(), memory_order_relaxed);
    atomic_store_explicit(&hl_private_fork_armed, 1, memory_order_relaxed);
    free(hl_private_fork_cells);
    hl_private_fork_cells = NULL;
    hl_private_fork_count = 0;
    size_t capacity = 0;
    /* Only this process's row belongs to the fork snapshot.  Other engine
       processes legitimately mutate the shared registry at any time; a
       registry-wide epoch check made those unrelated changes surface as a
       spurious guest fork EAGAIN.  The row generation and identity checks
       below detect every mutation that can affect this snapshot, while the
       process-local fork lock serializes this process's own descriptor
       mutations until fork_complete. */
    for (unsigned record = 0; record < HL_PRIVATE_PROCESSES; ++record) {
        hl_private_process *process = &hl_private[record];
        if (atomic_load_explicit(&process->state, memory_order_acquire) != HL_PRIVATE_LIVE ||
            atomic_load_explicit(&process->pid, memory_order_relaxed) != pid ||
            atomic_load_explicit(&process->start_ns, memory_order_relaxed) != start)
            continue;
        uint64_t generation = atomic_load_explicit(&process->generation, memory_order_acquire);
        for (unsigned index = 0; index < HL_PRIVATE_CELLS; ++index) {
            uint64_t cell = atomic_load_explicit(&process->cells[index], memory_order_acquire);
            if (cell == 0) continue;
            if (hl_private_fork_count == capacity) {
                size_t next = capacity ? capacity * 2 : 64;
                uint64_t *grown = realloc(hl_private_fork_cells, next * sizeof *grown);
                if (!grown) {
                    free(hl_private_fork_cells);
                    hl_private_fork_cells = NULL;
                    hl_private_fork_count = 0;
                    hl_private_fork_disarm();
                    (void)pthread_mutex_unlock(&hl_private_fork_lock);
                    return -ENOMEM;
                }
                hl_private_fork_cells = grown;
                capacity = next;
            }
            hl_private_fork_cells[hl_private_fork_count++] = cell;
        }
        if (atomic_load_explicit(&process->generation, memory_order_acquire) != generation ||
            atomic_load_explicit(&process->state, memory_order_acquire) != HL_PRIVATE_LIVE ||
            atomic_load_explicit(&process->pid, memory_order_relaxed) != pid ||
            atomic_load_explicit(&process->start_ns, memory_order_relaxed) != start) {
            free(hl_private_fork_cells);
            hl_private_fork_cells = NULL;
            hl_private_fork_count = 0;
            hl_private_fork_disarm();
            (void)pthread_mutex_unlock(&hl_private_fork_lock);
            return -EAGAIN;
        }
    }
    return 0;
}

int hl_host_process_fd_private_fork_complete(int child) {
    int result = 0;
    if (child) {
        for (size_t index = 0; index < hl_private_fork_count; ++index) {
            int fd = (int)((uint32_t)(hl_private_fork_cells[index] >> 32) - 1u);
            uint32_t references = (uint32_t)hl_private_fork_cells[index];
            for (uint32_t reference = 0; reference < references; ++reference) {
                result = hl_private_add_unlocked(fd);
                if (result != 0) break;
            }
            if (result != 0) break;
        }
        if (result != 0) hl_private_cleanup();
    }
    free(hl_private_fork_cells);
    hl_private_fork_cells = NULL;
    hl_private_fork_count = 0;
    hl_private_fork_disarm();
    (void)pthread_mutex_unlock(&hl_private_fork_lock);
    return result;
}

void hl_host_process_fd_private_cleanup(void) {
    hl_private_cleanup();
}

#if defined(HL_NATIVE_TEST_HOOKS)
#include <signal.h>
#include <sys/wait.h>
#if defined(__linux__)
#include <sys/prctl.h>
#endif

#define HL_PRIVATE_FORK_HOLD_NS 1500000000L
#define HL_PRIVATE_FORK_SETTLE_NS 200000000L
#define HL_PRIVATE_FORK_WAIT_MS 10000

static void hl_private_sleep(long nanoseconds) {
    struct timespec delay = {nanoseconds / 1000000000L, nanoseconds % 1000000000L};
    while (nanosleep(&delay, &delay) != 0 && errno == EINTR)
        continue;
}

/* Holds the fork lock for a window wide enough that a fork() started after it can only
   observe the lock held -- the same shape as the real hold across the /proc stat read. */
static void *hl_private_fork_lock_holder(void *argument) {
    (void)argument;
    if (pthread_mutex_lock(&hl_private_fork_lock) != 0) return NULL;
    hl_private_sleep(HL_PRIVATE_FORK_HOLD_NS);
    (void)pthread_mutex_unlock(&hl_private_fork_lock);
    return NULL;
}

/* Scenario 1: fork() while a sibling thread holds hl_private_fork_lock, and have the child
   take the locking path. Without the pthread_atfork child handler the child inherits a
   locked mutex whose owner does not exist in the child and blocks forever; the wait below
   is bounded so that regression is reported rather than wedging the caller. */
HL_API int hl_c_backend_private_fork_lock_test(uint32_t scenario) {
    if (scenario == 2) {
        pid_t child = fork();
        if (child == 0) {
            struct rlimit limit;
            if (getrlimit(RLIMIT_NOFILE, &limit) != 0) _exit(2);
            limit.rlim_cur = 1024;
            if (setrlimit(RLIMIT_NOFILE, &limit) != 0) _exit(3);
            atomic_store_explicit(&hl_private_guest_limit, 0, memory_order_release);
            hl_private_configure_limit();
            hl_private_floor_forget();
            if (getrlimit(RLIMIT_NOFILE, &limit) != 0) _exit(4);
            uint32_t guest = hl_engine_guest_fd_limit();
            int floor = hl_host_process_fd_private_floor();
            _exit(guest == 960u && floor == 960 &&
                          limit.rlim_cur >= (rlim_t)HL_LINUX_FD_LIMIT + HL_HOST_PRIVATE_DESCRIPTOR_MINIMUM
                      ? 0
                      : 5);
        }
        int status = 0;
        if (child < 0 || waitpid(child, &status, 0) != child) return -errno;
        return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -ERANGE;
    }
    if (scenario == 3) {
        pid_t child = fork();
        if (child == 0) {
            struct rlimit low = {96, 96};
            if (setrlimit(RLIMIT_NOFILE, &low) != 0) _exit(2);
            atomic_store_explicit(&hl_private_guest_limit, 0, memory_order_release);
            hl_private_configure_limit();
            hl_private_floor_forget();
            uint32_t guest = hl_engine_guest_fd_limit();
            int floor = hl_host_process_fd_private_floor();
            _exit(guest == 10u && floor == 10 ? 0 : 3);
        }
        int status = 0;
        if (child < 0 || waitpid(child, &status, 0) != child) return -errno;
        return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : -ERANGE;
    }
    if (scenario != 1) {
        errno = EINVAL;
        return -EINVAL;
    }
    hl_host_private_init();
    int descriptor = dup(STDERR_FILENO);
    if (descriptor < 0) return -errno;
    pthread_t holder;
    if (pthread_create(&holder, NULL, hl_private_fork_lock_holder, NULL) != 0) {
        close(descriptor);
        return -EAGAIN;
    }
    hl_private_sleep(HL_PRIVATE_FORK_SETTLE_NS);
    pid_t child = fork();
    if (child == 0) {
#if defined(__linux__)
        (void)prctl(PR_SET_PDEATHSIG, SIGKILL);
#endif
        /* A wedged child must not orphan: one survived its lane by 17 minutes. */
        (void)alarm(30);
        _exit(hl_host_process_fd_private_add(descriptor) == 0 ? 0 : 3);
    }
    int result;
    if (child < 0) {
        result = -errno;
    } else {
        int status = 0;
        int reaped = 0;
        for (int elapsed = 0; elapsed < HL_PRIVATE_FORK_WAIT_MS; elapsed += 20) {
            pid_t seen = waitpid(child, &status, WNOHANG);
            if (seen == child) {
                reaped = 1;
                break;
            }
            if (seen < 0 && errno != EINTR) break;
            hl_private_sleep(20000000L);
        }
        if (!reaped) {
            (void)kill(child, SIGKILL);
            (void)waitpid(child, &status, 0);
            result = -ETIMEDOUT;
        } else if (!WIFEXITED(status)) {
            result = -EINTR;
        } else {
            result = WEXITSTATUS(status) == 0 ? 0 : -EIO;
        }
    }
    (void)pthread_join(holder, NULL);
    hl_host_process_fd_private_remove(descriptor);
    close(descriptor);
    return result;
}
#endif
