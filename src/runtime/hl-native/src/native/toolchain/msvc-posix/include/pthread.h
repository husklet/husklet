/*
 * <pthread.h> for the x86_64-pc-windows-msvc target.
 *
 * The mingw-w64 lane gets this from winpthreads, a full POSIX threads
 * implementation shipped as a GNU static archive. That archive cannot be
 * linked into an MSVC-ABI image, so the subset this tree actually calls is
 * reimplemented over Win32 in posix.c. "The subset this tree actually calls"
 * is not a guess: it is the exact set of pthread_* identifiers that the
 * archive's source closure leaves undeclared when compiled against the MSVC
 * headers, and nothing else is declared here. A call site that grows a new
 * one gets a compile error naming it, which is the intended failure.
 *
 * The mapping, and why each choice rather than the obvious alternative:
 *
 *   pthread_mutex_t   -> SRWLOCK. Not CRITICAL_SECTION, which cannot be
 *                        statically initialised: PTHREAD_MUTEX_INITIALIZER has
 *                        to be a brace initialiser usable at file scope, and
 *                        SRWLOCK_INIT is one. SRW locks are also not recursive,
 *                        which matches the default POSIX mutex type -- and this
 *                        tree never sets PTHREAD_MUTEX_RECURSIVE on a mutex
 *                        reachable here.
 *   pthread_cond_t    -> CONDITION_VARIABLE, which pairs with SRWLOCK through
 *                        SleepConditionVariableSRW and is likewise statically
 *                        initialisable.
 *   pthread_once_t    -> a plain LONG driven by an interlocked compare and a
 *                        spin, rather than INIT_ONCE. INIT_ONCE would be the
 *                        natural pick, but its initialiser macro expands to a
 *                        struct with a pointer member, and PTHREAD_ONCE_INIT is
 *                        used here in contexts that want a scalar zero.
 *   pthread_key_t     -> the TLS slot index from TlsAlloc. Destructors are NOT
 *                        supported: Win32 fibre-local storage has no per-thread
 *                        destructor callback outside a DLL's thread-detach
 *                        notification, and this archive is not a DLL. See the
 *                        note on pthread_key_create in posix.c -- passing a
 *                        non-NULL destructor FAILS rather than silently
 *                        ignoring it.
 *   pthread_t         -> the Win32 thread id plus its handle. An id alone would
 *                        make pthread_join impossible without reopening the
 *                        thread, and a handle alone would make pthread_equal
 *                        wrong, since two handles to one thread compare
 *                        unequal.
 *
 * Process-shared synchronisation is REFUSED, not faked: pthread_mutexattr_
 * setpshared and pthread_condattr_setpshared return ENOTSUP for
 * PTHREAD_PROCESS_SHARED. No Win32 primitive here is placeable in shared
 * memory, and a caller that asked for cross-process safety and silently got
 * per-process safety would corrupt state rather than fail. winpthreads makes
 * the same refusal, so the two Windows lanes behave identically.
 */

#ifndef HL_MSVC_POSIX_PTHREAD_H
#define HL_MSVC_POSIX_PTHREAD_H

#include <sched.h>
#include <sys/types.h>
#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Win32 opaque shapes, declared by value rather than by including <windows.h>.
 * Both SRWLOCK and CONDITION_VARIABLE are documented as a single pointer-sized
 * opaque field, and both are initialised to all-zero by SRWLOCK_INIT /
 * CONDITION_VARIABLE_INIT. Declaring them structurally keeps the Win32
 * preprocessor vocabulary out of every TU that locks a mutex; posix.c includes
 * the real <windows.h> and asserts the sizes match. */
typedef struct {
    void *opaque;
} pthread_mutex_t;

typedef struct {
    void *opaque;
} pthread_cond_t;

typedef struct {
    int pshared;
    int type;
} pthread_mutexattr_t;

typedef struct {
    int pshared;
} pthread_condattr_t;

typedef unsigned long pthread_key_t;
typedef long pthread_once_t;

typedef struct {
    unsigned long id;
    void *handle;
} pthread_t;

#define PTHREAD_MUTEX_INITIALIZER {0}
#define PTHREAD_COND_INITIALIZER {0}
#define PTHREAD_ONCE_INIT 0

#define PTHREAD_PROCESS_PRIVATE 0
#define PTHREAD_PROCESS_SHARED 1

#define PTHREAD_MUTEX_NORMAL 0
#define PTHREAD_MUTEX_RECURSIVE 1
#define PTHREAD_MUTEX_ERRORCHECK 2
#define PTHREAD_MUTEX_DEFAULT PTHREAD_MUTEX_NORMAL

#define PTHREAD_CREATE_JOINABLE 0
#define PTHREAD_CREATE_DETACHED 1

int pthread_mutex_init(pthread_mutex_t *mutex, const pthread_mutexattr_t *attr);
int pthread_mutex_destroy(pthread_mutex_t *mutex);
int pthread_mutex_lock(pthread_mutex_t *mutex);
int pthread_mutex_trylock(pthread_mutex_t *mutex);
int pthread_mutex_unlock(pthread_mutex_t *mutex);

int pthread_mutexattr_init(pthread_mutexattr_t *attr);
int pthread_mutexattr_destroy(pthread_mutexattr_t *attr);
int pthread_mutexattr_setpshared(pthread_mutexattr_t *attr, int pshared);
int pthread_mutexattr_settype(pthread_mutexattr_t *attr, int type);

int pthread_cond_init(pthread_cond_t *cond, const pthread_condattr_t *attr);
int pthread_cond_destroy(pthread_cond_t *cond);
int pthread_cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex);
int pthread_cond_timedwait(pthread_cond_t *cond, pthread_mutex_t *mutex, const struct timespec *deadline);
int pthread_cond_signal(pthread_cond_t *cond);
int pthread_cond_broadcast(pthread_cond_t *cond);

int pthread_condattr_init(pthread_condattr_t *attr);
int pthread_condattr_destroy(pthread_condattr_t *attr);
int pthread_condattr_setpshared(pthread_condattr_t *attr, int pshared);

int pthread_key_create(pthread_key_t *key, void (*destructor)(void *));
int pthread_key_delete(pthread_key_t key);
void *pthread_getspecific(pthread_key_t key);
int pthread_setspecific(pthread_key_t key, const void *value);

int pthread_once(pthread_once_t *control, void (*routine)(void));

int pthread_create(pthread_t *thread, const void *attr, void *(*start)(void *), void *argument);
int pthread_join(pthread_t thread, void **result);
int pthread_detach(pthread_t thread);
pthread_t pthread_self(void);
int pthread_equal(pthread_t left, pthread_t right);
int pthread_kill(pthread_t thread, int signal_number);
int pthread_atfork(void (*prepare)(void), void (*parent)(void), void (*child)(void));

#ifdef __cplusplus
}
#endif

#endif /* HL_MSVC_POSIX_PTHREAD_H */
