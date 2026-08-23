/* Atomic publication boundary for namespace objects and their virtual metadata.
 *
 * Writers serialize through a process-owned POSIX record lock and publish an
 * odd sequence while a namespace mutation is incomplete. Readers normally do
 * two atomic loads: resolve the host object and virtual metadata between
 * read_begin/read_validate, then retry if a writer crossed that interval.
 * They take the record lock only after observing an active writer. No reader
 * lock is retained while resolving a pathname or performing blocking I/O.
 */

#include <errno.h>
#include <limits.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#if !defined(_WIN32)
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#endif

#include "../../../host/system.h"
#include "../../host_mman.h"
#include "../../memory_arena.h"
#include "namespace_transaction.h"

#ifndef HL_TARGET_LOCAL
#define HL_TARGET_LOCAL(name) name
#endif

struct namespace_transaction_state {
    _Atomic uint64_t sequence;
    _Atomic uint64_t owner;
    _Atomic uint64_t next_owner;
    _Atomic uint32_t poisoned;
};

static struct namespace_transaction_state *g_namespace_transaction;
static int g_namespace_transaction_fd = -1;
static atomic_flag g_namespace_transaction_local = ATOMIC_FLAG_INIT;
static _Thread_local uint32_t g_namespace_transaction_depth;
static _Thread_local uint64_t g_namespace_transaction_writer_generation;
static _Thread_local uint64_t g_namespace_transaction_writer_identity;
static _Thread_local int g_namespace_transaction_cancel_state;

enum namespace_transaction_lock { NAMESPACE_UNLOCK, NAMESPACE_READ_LOCK, NAMESPACE_WRITE_LOCK };

static int64_t namespace_transaction_now_ns(void) {
#if defined(_WIN32)
    /* The Windows implementation does not currently use a kernel record lock;
     * a finite spin budget below still bounds local contention. */
    return 0;
#else
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) return -1;
    return (int64_t)now.tv_sec * INT64_C(1000000000) + now.tv_nsec;
#endif
}

static int namespace_transaction_local_lock(void) {
#if defined(_WIN32)
    for (unsigned attempt = 0; attempt < 65536; ++attempt) {
        if (!atomic_flag_test_and_set_explicit(&g_namespace_transaction_local, memory_order_acquire)) return 0;
        atomic_signal_fence(memory_order_seq_cst);
    }
#else
    int64_t started = namespace_transaction_now_ns();
    if (started < 0) return -1;
    for (;;) {
        if (!atomic_flag_test_and_set_explicit(&g_namespace_transaction_local, memory_order_acquire)) return 0;
        int64_t now = namespace_transaction_now_ns();
        if (now < 0) return -1;
        if (now - started >= INT64_C(1000000000)) break;
        struct timespec pause = {.tv_sec = 0, .tv_nsec = 1000000};
        (void)nanosleep(&pause, NULL);
    }
#endif
    return errno = ETIMEDOUT, -1;
}

static int namespace_transaction_record_lock(enum namespace_transaction_lock requested) {
#if defined(_WIN32)
    (void)requested;
    return 0;
#else
    short type = requested == NAMESPACE_UNLOCK ? F_UNLCK : requested == NAMESPACE_READ_LOCK ? F_RDLCK : F_WRLCK;
    struct flock lock = {.l_type = type, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    if (requested == NAMESPACE_UNLOCK) return fcntl(g_namespace_transaction_fd, F_SETLK, &lock);
    int64_t started = namespace_transaction_now_ns();
    if (started < 0) return -1;
    for (;;) {
        if (fcntl(g_namespace_transaction_fd, F_SETLK, &lock) == 0) return 0;
        if (errno != EACCES && errno != EAGAIN && errno != EINTR) return -1;
        int64_t now = namespace_transaction_now_ns();
        if (now < 0) return -1;
        if (now - started >= INT64_C(1000000000)) return errno = ETIMEDOUT, -1;
        struct timespec pause = {.tv_sec = 0, .tv_nsec = 1000000};
        (void)nanosleep(&pause, NULL);
    }
#endif
}

static int namespace_transaction_cancel_disable(int *prior) {
#if defined(_WIN32)
    (void)prior;
    return 0;
#else
    int status = pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, prior);
    return status == 0 ? 0 : (errno = status, -1);
#endif
}

static void namespace_transaction_cancel_restore(int prior) {
#if !defined(_WIN32)
    if (pthread_setcancelstate(prior, NULL) != 0) abort();
#else
    (void)prior;
#endif
}

#if !defined(_WIN32)
static pthread_once_t g_namespace_transaction_atfork_once = PTHREAD_ONCE_INIT;

static void namespace_transaction_register_atfork(void) {
    (void)pthread_atfork(NULL, NULL, namespace_transaction_fork_child);
}
#endif

static int namespace_transaction_init(const hl_host_services *host) {
    if (g_namespace_transaction != NULL) return 0;
#if defined(_WIN32)
    /* A process-owned replacement for POSIX record locks is not implemented.
     * Fail closed instead of advertising cross-process atomic publication. */
    (void)host;
    return errno = ENOTSUP, -1;
#else
    void *arena = NULL;
    int descriptor = -1;
    char path[] = "/tmp/.hl-namespace-XXXXXX";
    if (pthread_once(&g_namespace_transaction_atfork_once, namespace_transaction_register_atfork) != 0)
        return errno = EIO, -1;
    descriptor = mkstemp(path);
    if (descriptor < 0) return -1;
    (void)unlink(path);
    int adopted = hl_host_process_fd_private_adopt(descriptor);
    if (adopted < 0) {
        int saved = -adopted;
        close(descriptor);
        return errno = saved, -1;
    }
    descriptor = adopted;
    int flags = fcntl(descriptor, F_GETFD);
    if (flags < 0 || fcntl(descriptor, F_SETFD, flags | FD_CLOEXEC) != 0) {
        int saved = errno;
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        return errno = saved, -1;
    }
    if (hl_linux_shared_create(host, sizeof(struct namespace_transaction_state), &arena) != HL_STATUS_OK) {
        hl_host_process_fd_private_remove(descriptor);
        close(descriptor);
        return errno = ENOMEM, -1;
    }
    g_namespace_transaction_fd = descriptor;
    g_namespace_transaction = arena;
    atomic_store_explicit(&g_namespace_transaction->sequence, 0, memory_order_relaxed);
    atomic_store_explicit(&g_namespace_transaction->owner, 0, memory_order_relaxed);
    atomic_store_explicit(&g_namespace_transaction->next_owner, 0, memory_order_relaxed);
    atomic_store_explicit(&g_namespace_transaction->poisoned, 0, memory_order_relaxed);
    return 0;
#endif
}

static int namespace_transaction_begin(void) {
    if (g_namespace_transaction_depth != 0) {
        ++g_namespace_transaction_depth;
        return 0;
    }
    if (g_namespace_transaction == NULL) return errno = EIO, -1;
#if !defined(_WIN32)
    if (g_namespace_transaction_fd < 0) return errno = EIO, -1;
#endif
    if (namespace_transaction_cancel_disable(&g_namespace_transaction_cancel_state) != 0) return -1;
    if (namespace_transaction_local_lock() != 0) {
        int saved = errno;
        namespace_transaction_cancel_restore(g_namespace_transaction_cancel_state);
        return errno = saved, -1;
    }
    if (namespace_transaction_record_lock(NAMESPACE_WRITE_LOCK) != 0) {
        int saved = errno;
        atomic_flag_clear_explicit(&g_namespace_transaction_local, memory_order_release);
        namespace_transaction_cancel_restore(g_namespace_transaction_cancel_state);
        return errno = saved, -1;
    }
    uint64_t sequence = atomic_load_explicit(&g_namespace_transaction->sequence, memory_order_acquire);
    if ((sequence & 1u) != 0 || atomic_load_explicit(&g_namespace_transaction->poisoned, memory_order_acquire)) {
        atomic_store_explicit(&g_namespace_transaction->poisoned, 1, memory_order_release);
        if (namespace_transaction_record_lock(NAMESPACE_UNLOCK) != 0) abort();
        atomic_flag_clear_explicit(&g_namespace_transaction_local, memory_order_release);
        namespace_transaction_cancel_restore(g_namespace_transaction_cancel_state);
        return errno = EOWNERDEAD, -1;
    }
    if (sequence == UINT64_MAX - 1u) {
        atomic_store_explicit(&g_namespace_transaction->poisoned, 1, memory_order_release);
        if (namespace_transaction_record_lock(NAMESPACE_UNLOCK) != 0) abort();
        atomic_flag_clear_explicit(&g_namespace_transaction_local, memory_order_release);
        namespace_transaction_cancel_restore(g_namespace_transaction_cancel_state);
        return errno = EOVERFLOW, -1;
    }
    uint64_t owner_sequence = atomic_load_explicit(&g_namespace_transaction->next_owner, memory_order_relaxed);
    if (owner_sequence == UINT64_MAX) {
        atomic_store_explicit(&g_namespace_transaction->poisoned, 1, memory_order_release);
        if (namespace_transaction_record_lock(NAMESPACE_UNLOCK) != 0) abort();
        atomic_flag_clear_explicit(&g_namespace_transaction_local, memory_order_release);
        namespace_transaction_cancel_restore(g_namespace_transaction_cancel_state);
        return errno = EOVERFLOW, -1;
    }
    uint64_t identity = owner_sequence + 1u;
    atomic_store_explicit(&g_namespace_transaction->next_owner, identity, memory_order_relaxed);
    atomic_store_explicit(&g_namespace_transaction->owner, identity, memory_order_relaxed);
    atomic_store_explicit(&g_namespace_transaction->sequence, sequence + 1u, memory_order_release);
    g_namespace_transaction_writer_generation = sequence + 1u;
    g_namespace_transaction_writer_identity = identity;
    g_namespace_transaction_depth = 1;
    return 0;
}

static void namespace_transaction_end(void) {
    if (g_namespace_transaction_depth == 0) abort();
    if (--g_namespace_transaction_depth != 0) return;
    uint64_t sequence = atomic_load_explicit(&g_namespace_transaction->sequence, memory_order_relaxed);
    if ((sequence & 1u) == 0 || sequence == UINT64_MAX) abort();
    atomic_store_explicit(&g_namespace_transaction->owner, 0, memory_order_relaxed);
    atomic_store_explicit(&g_namespace_transaction->sequence, sequence + 1u, memory_order_release);
    g_namespace_transaction_writer_generation = 0;
    g_namespace_transaction_writer_identity = 0;
    if (namespace_transaction_record_lock(NAMESPACE_UNLOCK) != 0) {
        atomic_store_explicit(&g_namespace_transaction->poisoned, 1, memory_order_release);
        abort();
    }
    atomic_flag_clear_explicit(&g_namespace_transaction_local, memory_order_release);
    namespace_transaction_cancel_restore(g_namespace_transaction_cancel_state);
}

/* Waits out an observed writer without retaining a lock. An odd sequence after
 * the record lock is acquired means the owning process died mid-publication. */
static int namespace_transaction_read_slow(struct namespace_transaction_read *read) {
    int prior = 0;
    if (namespace_transaction_cancel_disable(&prior) != 0) return -1;
    if (namespace_transaction_local_lock() != 0) {
        int saved = errno;
        namespace_transaction_cancel_restore(prior);
        return errno = saved, -1;
    }
    if (namespace_transaction_record_lock(NAMESPACE_READ_LOCK) != 0) {
        int saved = errno;
        atomic_flag_clear_explicit(&g_namespace_transaction_local, memory_order_release);
        namespace_transaction_cancel_restore(prior);
        return errno = saved, -1;
    }
    uint64_t sequence = atomic_load_explicit(&g_namespace_transaction->sequence, memory_order_acquire);
    int poisoned = atomic_load_explicit(&g_namespace_transaction->poisoned, memory_order_acquire) != 0;
    if ((sequence & 1u) != 0) {
        atomic_store_explicit(&g_namespace_transaction->poisoned, 1, memory_order_release);
        poisoned = 1;
    }
    if (namespace_transaction_record_lock(NAMESPACE_UNLOCK) != 0) abort();
    atomic_flag_clear_explicit(&g_namespace_transaction_local, memory_order_release);
    namespace_transaction_cancel_restore(prior);
    if (poisoned) return errno = EOWNERDEAD, -1;
    read->sequence = sequence;
    return 0;
}

static int namespace_transaction_read_begin(struct namespace_transaction_read *read) {
    if (read == NULL) return errno = EINVAL, -1;
    if (g_namespace_transaction == NULL) return errno = EIO, -1;
#if !defined(_WIN32)
    if (g_namespace_transaction_fd < 0) return errno = EIO, -1;
#endif
    if (atomic_load_explicit(&g_namespace_transaction->poisoned, memory_order_acquire)) return errno = EOWNERDEAD, -1;
    uint64_t sequence = atomic_load_explicit(&g_namespace_transaction->sequence, memory_order_acquire);
    if ((sequence & 1u) != 0) return namespace_transaction_read_slow(read);
    read->sequence = sequence;
    atomic_thread_fence(memory_order_acquire);
    return 0;
}

static int namespace_transaction_read_validate(const struct namespace_transaction_read *read) {
    if (read == NULL) return errno = EINVAL, -1;
    atomic_thread_fence(memory_order_acquire);
    if (atomic_load_explicit(&g_namespace_transaction->poisoned, memory_order_acquire)) return errno = EOWNERDEAD, -1;
    uint64_t sequence = atomic_load_explicit(&g_namespace_transaction->sequence, memory_order_acquire);
    return sequence == read->sequence && (sequence & 1u) == 0 ? 0 : (errno = EAGAIN, -1);
}

static int namespace_transaction_read_barrier(void) {
    struct namespace_transaction_read read;
    /* The caller owns retries around real lookup work. This zero-work helper
     * bounds its internal retries so continuous mutation fails closed rather
     * than trapping a guest syscall forever. Every API returns 0 or -1/errno. */
    for (unsigned attempt = 0; attempt < 64; ++attempt) {
        if (namespace_transaction_read_begin(&read) != 0) return -1;
        if (namespace_transaction_read_validate(&read) == 0) return 0;
        if (errno != EAGAIN) return -1;
    }
    return errno = EBUSY, -1;
}

/* Registered as the pthread_atfork child handler, so it runs in every child of
 * a host fork() before any other code in that child. Only the forking thread
 * survives, and it may have been mid-transaction: the child inherits that
 * thread's depth, its writer identity, and its disabled cancellation state,
 * while the POSIX record lock the transaction held is not inherited at all.
 * Reset the inherited transaction and hand cancellation back to whatever the
 * thread had before it entered.
 *
 * Restoring here rather than deferring it is what makes the child sound. No
 * single re-entry point exists that every child passes through -- guest
 * fork/vfork/clone, host spawn, and checkpoint restore all fork -- so a
 * deferred restore would be a restore that never happens. pthread_setcancelstate
 * writes this thread's own descriptor: it allocates nothing, takes no lock,
 * logs nothing, and cannot unwind while the cancellation type stays deferred,
 * which is the only type this file ever uses. */
static void namespace_transaction_fork_child(void) {
    int inherited_writer = g_namespace_transaction_depth != 0;
    g_namespace_transaction_depth = 0;
    g_namespace_transaction_writer_generation = 0;
    g_namespace_transaction_writer_identity = 0;
    atomic_flag_clear_explicit(&g_namespace_transaction_local, memory_order_release);
    if (inherited_writer) namespace_transaction_cancel_restore(g_namespace_transaction_cancel_state);
}

static int namespace_transaction_writer(struct namespace_transaction_writer *writer) {
    if (writer == NULL) return errno = EINVAL, -1;
    if (g_namespace_transaction == NULL || g_namespace_transaction_depth == 0 ||
        g_namespace_transaction_writer_generation == 0 || g_namespace_transaction_writer_identity == 0)
        return errno = EPERM, -1;
    uint64_t generation = atomic_load_explicit(&g_namespace_transaction->sequence, memory_order_acquire);
    uint64_t owner = atomic_load_explicit(&g_namespace_transaction->owner, memory_order_acquire);
    if (generation != g_namespace_transaction_writer_generation || owner != g_namespace_transaction_writer_identity ||
        (generation & 1u) == 0 || atomic_load_explicit(&g_namespace_transaction->poisoned, memory_order_acquire))
        return errno = EOWNERDEAD, -1;
    *writer = (struct namespace_transaction_writer){&g_namespace_transaction->sequence, &g_namespace_transaction->owner,
                                                    generation, owner};
    return 0;
}

static int namespace_transaction_namespace(_Atomic uint64_t **generation, _Atomic uint64_t **owner) {
    if (generation == NULL || owner == NULL) return errno = EINVAL, -1;
    if (g_namespace_transaction == NULL) return errno = EIO, -1;
    *generation = &g_namespace_transaction->sequence;
    *owner = &g_namespace_transaction->owner;
    return 0;
}

static void namespace_transaction_poison(void) {
    if (g_namespace_transaction == NULL || g_namespace_transaction_depth == 0) abort();
    atomic_store_explicit(&g_namespace_transaction->poisoned, 1, memory_order_release);
}

#if defined(HL_NATIVE_TEST_HOOKS) && !defined(_WIN32)
/* Deterministic primitive tests. Integration exports this narrow hook from the
 * native test ABI; production has no test surface. */
static int namespace_transaction_test_reset(void) {
    atomic_store_explicit(&g_namespace_transaction->sequence, 0, memory_order_relaxed);
    atomic_store_explicit(&g_namespace_transaction->owner, 0, memory_order_relaxed);
    atomic_store_explicit(&g_namespace_transaction->next_owner, 0, memory_order_relaxed);
    atomic_store_explicit(&g_namespace_transaction->poisoned, 0, memory_order_release);
    return 0;
}

static void *namespace_transaction_test_unowned_thread(void *argument) {
    struct namespace_transaction_writer writer;
    int *result = argument;
    *result = namespace_transaction_writer(&writer) < 0 ? errno : 0;
    return NULL;
}

struct namespace_transaction_test_fork_arguments {
    int ready[2];
    int proceed[2];
    int result;
};

/* Holds the writer, forks, and lets the parent side release the transaction only
 * after the child has reported on the state it inherited. */
static void *namespace_transaction_test_fork_thread(void *argument) {
    struct namespace_transaction_test_fork_arguments *arguments = argument;
    if (namespace_transaction_begin() != 0) return arguments->result = 70, NULL;
    pid_t child = fork();
    if (child < 0) {
        namespace_transaction_end();
        return arguments->result = 71, NULL;
    }
    if (child == 0) {
        struct namespace_transaction_writer inherited;
        int restored = -1;
        if (pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &restored) != 0) _exit(72);
        if (restored != PTHREAD_CANCEL_ENABLE) _exit(73);
        if (pthread_setcancelstate(PTHREAD_CANCEL_ENABLE, NULL) != 0) _exit(74);
        if (namespace_transaction_writer(&inherited) == 0 || errno != EPERM) _exit(75);
        if (write(arguments->ready[1], "x", 1) != 1) _exit(76);
        char byte;
        if (read(arguments->proceed[0], &byte, 1) != 1) _exit(77);
        if (namespace_transaction_begin() != 0) _exit(78);
        if (namespace_transaction_writer(&inherited) != 0) _exit(85);
        namespace_transaction_end();
        _exit(0);
    }
    char byte;
    if (read(arguments->ready[0], &byte, 1) != 1) return arguments->result = 79, NULL;
    namespace_transaction_end();
    if (write(arguments->proceed[1], "x", 1) != 1) return arguments->result = 80, NULL;
    int status = 0;
    if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) return arguments->result = 81, NULL;
    arguments->result = WEXITSTATUS(status) == 0 ? 0 : WEXITSTATUS(status);
    return NULL;
}

HL_API int HL_TARGET_LOCAL(namespace_transaction_test)(uint32_t scenario) {
    struct namespace_transaction_read snapshot;
    if (g_namespace_transaction == NULL) {
        if (hl_target_services_bind(&g_target_services) != 0) return 100;
        if (namespace_transaction_init(effective_host_services()) != 0) return 101;
    }
    if (scenario == 0) {
        if (namespace_transaction_read_begin(&snapshot) != 0 || namespace_transaction_read_validate(&snapshot) != 0)
            return 11;
        if (namespace_transaction_begin() != 0) return 12;
        namespace_transaction_end();
        if (namespace_transaction_read_validate(&snapshot) == 0 || errno != EAGAIN) return 13;
        return namespace_transaction_read_barrier() == 0 ? 0 : 14;
    }
    if (scenario == 1) {
        pid_t child = fork();
        if (child < 0) return 20;
        if (child == 0) {
            if (namespace_transaction_begin() != 0) _exit(21);
            _exit(0);
        }
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 22;
        if (namespace_transaction_read_begin(&snapshot) == 0 || errno != EOWNERDEAD) return 23;
        return namespace_transaction_test_reset();
    }
    if (scenario == 2) {
        int entered[2], release[2];
        if (pipe(entered) != 0 || pipe(release) != 0) return 30;
        pid_t child = fork();
        if (child < 0) return 31;
        if (child == 0) {
            close(entered[0]);
            close(release[1]);
            if (namespace_transaction_begin() != 0 || write(entered[1], "x", 1) != 1) _exit(32);
            char byte;
            if (read(release[0], &byte, 1) != 1) _exit(33);
            namespace_transaction_end();
            _exit(0);
        }
        close(entered[1]);
        close(release[0]);
        char byte;
        if (read(entered[0], &byte, 1) != 1) return 34;
        int result = namespace_transaction_read_begin(&snapshot);
        int saved = errno;
        if (write(release[1], "x", 1) != 1) return 35;
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 36;
        return result < 0 && saved == ETIMEDOUT ? 0 : 37;
    }
    if (scenario == 3) {
        int ready[2], proceed[2];
        if (pipe(ready) != 0 || pipe(proceed) != 0) return 40;
        if (namespace_transaction_begin() != 0) return 41;
        pid_t child = fork();
        if (child < 0) {
            namespace_transaction_end();
            return 42;
        }
        if (child == 0) {
            struct namespace_transaction_writer inherited;
            int restored = -1;
            close(ready[0]);
            close(proceed[1]);
            /* The child of a fork taken while this thread held the writer. The
             * atfork child handler must already have run: cancellation is back
             * to the enabled state the thread had before begin(), and the
             * inherited depth and writer identity are gone. */
            if (pthread_setcancelstate(PTHREAD_CANCEL_DISABLE, &restored) != 0) _exit(43);
            if (restored != PTHREAD_CANCEL_ENABLE) _exit(44);
            if (pthread_setcancelstate(PTHREAD_CANCEL_ENABLE, NULL) != 0) _exit(45);
            if (namespace_transaction_writer(&inherited) == 0 || errno != EPERM) _exit(46);
            if (write(ready[1], "x", 1) != 1) _exit(47);
            char byte;
            if (read(proceed[0], &byte, 1) != 1) _exit(48);
            /* A begin() that only incremented an inherited depth would return 0
             * without ever publishing a generation or taking the record lock. */
            if (namespace_transaction_begin() != 0) _exit(49);
            if (namespace_transaction_writer(&inherited) != 0) _exit(50);
            namespace_transaction_end();
            _exit(0);
        }
        close(ready[1]);
        close(proceed[0]);
        char byte;
        if (read(ready[0], &byte, 1) != 1) {
            int failed = 0;
            (void)waitpid(child, &failed, 0);
            return WIFEXITED(failed) ? WEXITSTATUS(failed) : 66;
        }
        namespace_transaction_end();
        if (write(proceed[1], "x", 1) != 1) return 67;
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status)) return 68;
        return WEXITSTATUS(status);
    }
    if (scenario == 4) {
        struct namespace_transaction_writer first, nested, second;
        if (namespace_transaction_begin() != 0 || namespace_transaction_writer(&first) != 0) return 50;
        if (first.writer_generation == 0 || first.writer_identity == 0) return 51;
        if (namespace_transaction_begin() != 0 || namespace_transaction_writer(&nested) != 0) return 52;
        if (nested.writer_generation != first.writer_generation || nested.writer_identity != first.writer_identity)
            return 53;
        namespace_transaction_end();
        if (namespace_transaction_writer(&nested) != 0 || nested.writer_identity != first.writer_identity) return 54;
        int thread_result = 0;
        pthread_t thread;
        if (pthread_create(&thread, NULL, namespace_transaction_test_unowned_thread, &thread_result) != 0 ||
            pthread_join(thread, NULL) != 0 || thread_result != EPERM)
            return 55;
        namespace_transaction_end();
        if (namespace_transaction_writer(&nested) == 0 || errno != EPERM ||
            atomic_load_explicit(&g_namespace_transaction->owner, memory_order_acquire) != 0)
            return 56;
        if (namespace_transaction_begin() != 0 || namespace_transaction_writer(&second) != 0) return 57;
        if (second.writer_generation == first.writer_generation || second.writer_identity == first.writer_identity)
            return 58;
        namespace_transaction_end();
        int values[2];
        if (pipe(values) != 0) return 59;
        pid_t child = fork();
        if (child < 0) return 60;
        if (child == 0) {
            struct namespace_transaction_writer child_writer;
            close(values[0]);
            if (namespace_transaction_writer(&child_writer) == 0 || errno != EPERM ||
                namespace_transaction_begin() != 0 || namespace_transaction_writer(&child_writer) != 0)
                _exit(61);
            uint64_t child_identity = child_writer.writer_identity;
            namespace_transaction_end();
            if (write(values[1], &child_identity, sizeof child_identity) != (ssize_t)sizeof child_identity) _exit(62);
            _exit(0);
        }
        close(values[1]);
        uint64_t child_identity = 0;
        if (read(values[0], &child_identity, sizeof child_identity) != (ssize_t)sizeof child_identity) return 63;
        int status = 0;
        if (waitpid(child, &status, 0) != child || !WIFEXITED(status) || WEXITSTATUS(status) != 0) return 64;
        return child_identity != 0 && child_identity != second.writer_identity ? 0 : 65;
    }
    if (scenario == 5) {
        struct namespace_transaction_test_fork_arguments arguments = {{-1, -1}, {-1, -1}, 0};
        pthread_t thread;
        if (pipe(arguments.ready) != 0 || pipe(arguments.proceed) != 0) return 82;
        if (pthread_create(&thread, NULL, namespace_transaction_test_fork_thread, &arguments) != 0) return 83;
        if (pthread_join(thread, NULL) != 0) return 84;
        close(arguments.ready[0]);
        close(arguments.ready[1]);
        close(arguments.proceed[0]);
        close(arguments.proceed[1]);
        return arguments.result;
    }
    return 99;
}
#elif defined(HL_NATIVE_TEST_HOOKS)
/* The scenarios below fork, wait and rendezvous through a MAP_SHARED page, none of which the Windows
 * target spells. The loader resolves every symbol named in the test export manifest, so the hook has to
 * exist here: a guarded block with no Windows arm links as "cannot export ...: symbol not defined" with
 * the hooks on, and would otherwise have been a MissingBridge at load rather than a compile error. */
HL_API int HL_TARGET_LOCAL(namespace_transaction_test)(uint32_t scenario);

HL_API int HL_TARGET_LOCAL(namespace_transaction_test)(uint32_t scenario) {
    (void)scenario;
    errno = ENOTSUP;
    return -1;
}
#endif
