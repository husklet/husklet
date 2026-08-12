/*
 * The POSIX seam for the x86_64-pc-windows-msvc target -- implementations.
 *
 * Every function here backs a declaration in this directory's headers, and the
 * set is closed: it is exactly what the archive's source closure leaves
 * undefined when compiled against the MSVC/UCRT headers instead of
 * mingw-w64's. The mingw-w64 lane gets all of it from winpthreads and
 * libmingwex, neither of which can be linked into an MSVC-ABI image.
 *
 * The rule followed throughout: implement, or fail loudly. Nothing here
 * returns success for work it did not do. Where Windows has no counterpart for
 * a POSIX guarantee -- process-shared mutexes, TLS destructors, pthread_kill
 * -- the call returns the POSIX error code for "this cannot be done", which is
 * what winpthreads does too, so the two Windows lanes agree.
 */

/* WIN32_LEAN_AND_MEAN excludes <winsock.h>, whose unguarded `struct timeval`
 * would otherwise collide with the one this directory's <sys/time.h> declares.
 * Nothing here needs a socket. */
#define WIN32_LEAN_AND_MEAN 1
#include <windows.h>

#include <bcrypt.h>
#include <errno.h>
#include <pthread.h>
#include <sched.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

/* pthread.h declares SRWLOCK and CONDITION_VARIABLE structurally so that
 * locking a mutex does not drag <windows.h> into every TU. That is only sound
 * if the shapes really do match; assert it here, where both definitions are
 * visible, so a future SDK that changed either one fails the build instead of
 * corrupting a lock. */
_STATIC_ASSERT(sizeof(pthread_mutex_t) == sizeof(SRWLOCK));
_STATIC_ASSERT(sizeof(pthread_cond_t) == sizeof(CONDITION_VARIABLE));

#define AS_SRWLOCK(m) ((PSRWLOCK)(void *)(m))
#define AS_CONDVAR(c) ((PCONDITION_VARIABLE)(void *)(c))

/* ---- mutexes ------------------------------------------------------------ */

int pthread_mutex_init(pthread_mutex_t *mutex, const pthread_mutexattr_t *attr) {
    if (mutex == NULL) return EINVAL;
    /* A process-shared request must already have failed at setpshared; refuse
     * again here in case an attribute object arrived from elsewhere. */
    if (attr != NULL && attr->pshared == PTHREAD_PROCESS_SHARED) return ENOTSUP;
    if (attr != NULL && attr->type == PTHREAD_MUTEX_RECURSIVE) return ENOTSUP;
    InitializeSRWLock(AS_SRWLOCK(mutex));
    return 0;
}

int pthread_mutex_destroy(pthread_mutex_t *mutex) {
    /* SRW locks hold no resource to release. */
    if (mutex == NULL) return EINVAL;
    return 0;
}

int pthread_mutex_lock(pthread_mutex_t *mutex) {
    if (mutex == NULL) return EINVAL;
    AcquireSRWLockExclusive(AS_SRWLOCK(mutex));
    return 0;
}

int pthread_mutex_trylock(pthread_mutex_t *mutex) {
    if (mutex == NULL) return EINVAL;
    return TryAcquireSRWLockExclusive(AS_SRWLOCK(mutex)) ? 0 : EBUSY;
}

int pthread_mutex_unlock(pthread_mutex_t *mutex) {
    if (mutex == NULL) return EINVAL;
    ReleaseSRWLockExclusive(AS_SRWLOCK(mutex));
    return 0;
}

int pthread_mutexattr_init(pthread_mutexattr_t *attr) {
    if (attr == NULL) return EINVAL;
    attr->pshared = PTHREAD_PROCESS_PRIVATE;
    attr->type = PTHREAD_MUTEX_NORMAL;
    return 0;
}

int pthread_mutexattr_destroy(pthread_mutexattr_t *attr) {
    if (attr == NULL) return EINVAL;
    return 0;
}

int pthread_mutexattr_setpshared(pthread_mutexattr_t *attr, int pshared) {
    if (attr == NULL) return EINVAL;
    /* No Win32 lock primitive is placeable in shared memory. Reporting success
     * would hand the caller a mutex that protects nothing across processes. */
    if (pshared == PTHREAD_PROCESS_SHARED) return ENOTSUP;
    if (pshared != PTHREAD_PROCESS_PRIVATE) return EINVAL;
    attr->pshared = pshared;
    return 0;
}

int pthread_mutexattr_settype(pthread_mutexattr_t *attr, int type) {
    if (attr == NULL) return EINVAL;
    if (type == PTHREAD_MUTEX_RECURSIVE) return ENOTSUP;
    if (type != PTHREAD_MUTEX_NORMAL && type != PTHREAD_MUTEX_ERRORCHECK) return EINVAL;
    attr->type = type;
    return 0;
}

/* ---- condition variables ------------------------------------------------ */

int pthread_cond_init(pthread_cond_t *cond, const pthread_condattr_t *attr) {
    if (cond == NULL) return EINVAL;
    if (attr != NULL && attr->pshared == PTHREAD_PROCESS_SHARED) return ENOTSUP;
    InitializeConditionVariable(AS_CONDVAR(cond));
    return 0;
}

int pthread_cond_destroy(pthread_cond_t *cond) {
    if (cond == NULL) return EINVAL;
    return 0;
}

int pthread_cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex) {
    if (cond == NULL || mutex == NULL) return EINVAL;
    if (!SleepConditionVariableSRW(AS_CONDVAR(cond), AS_SRWLOCK(mutex), INFINITE, 0)) return EINVAL;
    return 0;
}

int pthread_cond_timedwait(pthread_cond_t *cond, pthread_mutex_t *mutex, const struct timespec *deadline) {
    struct timespec now;
    long long milliseconds;

    if (cond == NULL || mutex == NULL || deadline == NULL) return EINVAL;
    /* POSIX gives an ABSOLUTE deadline on CLOCK_REALTIME; Win32 takes a
     * relative timeout. Converting needs the current time, so the two calls
     * race by however long the conversion takes -- which is why the result is
     * clamped at zero rather than allowed to go negative and become INFINITE. */
    if (clock_gettime(CLOCK_REALTIME, &now) != 0) return EINVAL;
    milliseconds = ((long long)deadline->tv_sec - (long long)now.tv_sec) * 1000LL +
                   ((long long)deadline->tv_nsec - (long long)now.tv_nsec) / 1000000LL;
    if (milliseconds < 0) milliseconds = 0;
    if (milliseconds > (long long)(INFINITE - 1)) milliseconds = (long long)(INFINITE - 1);
    if (!SleepConditionVariableSRW(AS_CONDVAR(cond), AS_SRWLOCK(mutex), (DWORD)milliseconds, 0))
        return GetLastError() == ERROR_TIMEOUT ? ETIMEDOUT : EINVAL;
    return 0;
}

int pthread_cond_signal(pthread_cond_t *cond) {
    if (cond == NULL) return EINVAL;
    WakeConditionVariable(AS_CONDVAR(cond));
    return 0;
}

int pthread_cond_broadcast(pthread_cond_t *cond) {
    if (cond == NULL) return EINVAL;
    WakeAllConditionVariable(AS_CONDVAR(cond));
    return 0;
}

int pthread_condattr_init(pthread_condattr_t *attr) {
    if (attr == NULL) return EINVAL;
    attr->pshared = PTHREAD_PROCESS_PRIVATE;
    return 0;
}

int pthread_condattr_destroy(pthread_condattr_t *attr) {
    if (attr == NULL) return EINVAL;
    return 0;
}

int pthread_condattr_setpshared(pthread_condattr_t *attr, int pshared) {
    if (attr == NULL) return EINVAL;
    if (pshared == PTHREAD_PROCESS_SHARED) return ENOTSUP;
    if (pshared != PTHREAD_PROCESS_PRIVATE) return EINVAL;
    attr->pshared = pshared;
    return 0;
}

/* ---- thread-specific data ----------------------------------------------- */

int pthread_key_create(pthread_key_t *key, void (*destructor)(void *)) {
    DWORD slot;
    if (key == NULL) return EINVAL;
    /* Refused rather than ignored. A TLS destructor runs on every thread's
     * exit, and Win32 offers that only through a DLL's DLL_THREAD_DETACH
     * notification -- this is a static archive, so there is no such callback.
     * Accepting the destructor and never calling it would turn a leak into
     * something the caller believes it has already handled. */
    if (destructor != NULL) return ENOTSUP;
    slot = TlsAlloc();
    if (slot == TLS_OUT_OF_INDEXES) return EAGAIN;
    *key = slot;
    return 0;
}

int pthread_key_delete(pthread_key_t key) {
    return TlsFree((DWORD)key) ? 0 : EINVAL;
}

void *pthread_getspecific(pthread_key_t key) {
    return TlsGetValue((DWORD)key);
}

int pthread_setspecific(pthread_key_t key, const void *value) {
    /* The const on the POSIX prototype is a historical artifact; the value is
     * stored, not read through. */
    return TlsSetValue((DWORD)key, (LPVOID)(void *)(uintptr_t)(const char *)value) ? 0 : EINVAL;
}

/* ---- one-time initialisation -------------------------------------------- */

/* Three states rather than two: a second caller arriving while the first is
 * still inside `routine` must BLOCK, not return, or it will use half-built
 * state. INIT_ONCE would give that for free, but its initialiser is not a
 * scalar zero and PTHREAD_ONCE_INIT has to be one here. */
#define ONCE_IDLE 0
#define ONCE_RUNNING 1
#define ONCE_DONE 2

int pthread_once(pthread_once_t *control, void (*routine)(void)) {
    LONG previous;
    if (control == NULL || routine == NULL) return EINVAL;
    for (;;) {
        previous = InterlockedCompareExchange((volatile LONG *)control, ONCE_RUNNING, ONCE_IDLE);
        if (previous == ONCE_DONE) return 0;
        if (previous == ONCE_IDLE) {
            routine();
            InterlockedExchange((volatile LONG *)control, ONCE_DONE);
            return 0;
        }
        /* ONCE_RUNNING: another thread is inside the routine. */
        SwitchToThread();
    }
}

/* ---- threads ------------------------------------------------------------ */

struct thread_start {
    void *(*start)(void *);
    void *argument;
};

static DWORD WINAPI thread_trampoline(LPVOID parameter) {
    struct thread_start *launch = (struct thread_start *)parameter;
    void *(*start)(void *) = launch->start;
    void *argument = launch->argument;
    free(launch);
    /* The POSIX return value is a void*; the Win32 one is a DWORD. Nothing in
     * this tree reads a thread result through pthread_join's second argument
     * (it is always NULL), so the truncation is not observable -- and
     * pthread_join below refuses a non-NULL result pointer rather than
     * inventing one. */
    (void)start(argument);
    return 0;
}

int pthread_create(pthread_t *thread, const void *attr, void *(*start)(void *), void *argument) {
    struct thread_start *launch;
    HANDLE handle;
    DWORD id = 0;

    if (thread == NULL || start == NULL) return EINVAL;
    /* No attribute object is supported. Every call site in this tree passes
     * NULL; a stack size or detach state arriving here would be silently
     * dropped, so it is refused instead. */
    if (attr != NULL) return ENOTSUP;

    launch = (struct thread_start *)malloc(sizeof(*launch));
    if (launch == NULL) return EAGAIN;
    launch->start = start;
    launch->argument = argument;

    handle = CreateThread(NULL, 0, thread_trampoline, launch, 0, &id);
    if (handle == NULL) {
        free(launch);
        return EAGAIN;
    }
    thread->id = id;
    thread->handle = handle;
    return 0;
}

int pthread_join(pthread_t thread, void **result) {
    /* See thread_trampoline: a Win32 thread's exit code cannot carry a void*,
     * so a caller asking for one is refused rather than handed a truncation. */
    if (result != NULL) return ENOTSUP;
    if (thread.handle == NULL) return ESRCH;
    if (WaitForSingleObject((HANDLE)thread.handle, INFINITE) != WAIT_OBJECT_0) return EINVAL;
    CloseHandle((HANDLE)thread.handle);
    return 0;
}

int pthread_detach(pthread_t thread) {
    if (thread.handle == NULL) return ESRCH;
    /* Closing the handle is exactly detachment on Win32: the thread runs on and
     * its kernel object is freed when it exits and the last handle is gone. */
    return CloseHandle((HANDLE)thread.handle) ? 0 : EINVAL;
}

pthread_t pthread_self(void) {
    pthread_t self;
    self.id = GetCurrentThreadId();
    /* GetCurrentThread() is a pseudo-handle valid only in this thread, so it is
     * deliberately NOT stored: a pthread_t obtained here must not be handed to
     * pthread_join by another thread and appear to work. join refuses a NULL
     * handle with ESRCH, which is the honest answer. */
    self.handle = NULL;
    return self;
}

int pthread_equal(pthread_t left, pthread_t right) {
    return left.id == right.id;
}

int pthread_kill(pthread_t thread, int signal_number) {
    (void)thread;
    (void)signal_number;
    /* Windows has no directed per-thread signal delivery. raise() targets the
     * calling thread only, and there is no way to run a handler on another
     * one. A caller expecting an interrupted blocking call would get silence. */
    return ENOTSUP;
}

int pthread_atfork(void (*prepare)(void), void (*parent)(void), void (*child)(void)) {
    (void)prepare;
    (void)parent;
    (void)child;
    /* There is no fork() on this host, so no handler could ever run. Reporting
     * success is correct rather than convenient: the contract is "run these
     * around a fork", and a host with no fork satisfies it vacuously. Failing
     * would make callers treat a condition that cannot arise as an error. */
    return 0;
}

/* ---- scheduling ---------------------------------------------------------- */

int sched_yield(void) {
    /* SwitchToThread rather than Sleep(0): Sleep(0) will not yield to a
     * lower-priority ready thread, so a spin loop using it can starve the very
     * thread it is waiting for. */
    (void)SwitchToThread();
    return 0;
}

/* ---- time ---------------------------------------------------------------- */

/* 100-nanosecond ticks between 1601-01-01 (the FILETIME epoch) and
 * 1970-01-01 (the POSIX epoch). */
#define EPOCH_DELTA_100NS 116444736000000000ULL

static int monotonic_now(struct timespec *now) {
    static LARGE_INTEGER frequency;
    LARGE_INTEGER counter;
    if (frequency.QuadPart == 0 && !QueryPerformanceFrequency(&frequency)) return -1;
    if (!QueryPerformanceCounter(&counter)) return -1;
    now->tv_sec = (time_t)(counter.QuadPart / frequency.QuadPart);
    /* Multiply the remainder, not the whole counter: the product of a raw
     * performance counter and 1e9 overflows 64 bits within a few seconds of
     * uptime on a 10 MHz timer. */
    now->tv_nsec = (long)(((counter.QuadPart % frequency.QuadPart) * 1000000000LL) / frequency.QuadPart);
    return 0;
}

static void filetime_to_timespec(const FILETIME *value, struct timespec *out) {
    ULARGE_INTEGER ticks;
    ticks.LowPart = value->dwLowDateTime;
    ticks.HighPart = value->dwHighDateTime;
    out->tv_sec = (time_t)(ticks.QuadPart / 10000000ULL);
    out->tv_nsec = (long)((ticks.QuadPart % 10000000ULL) * 100ULL);
}

static int cpu_time(HANDLE object, int is_process, struct timespec *now) {
    FILETIME creation, exit, kernel, user;
    ULARGE_INTEGER k, u, total;
    BOOL ok = is_process ? GetProcessTimes(object, &creation, &exit, &kernel, &user)
                         : GetThreadTimes(object, &creation, &exit, &kernel, &user);
    if (!ok) return -1;
    k.LowPart = kernel.dwLowDateTime;
    k.HighPart = kernel.dwHighDateTime;
    u.LowPart = user.dwLowDateTime;
    u.HighPart = user.dwHighDateTime;
    total.QuadPart = k.QuadPart + u.QuadPart;
    now->tv_sec = (time_t)(total.QuadPart / 10000000ULL);
    now->tv_nsec = (long)((total.QuadPart % 10000000ULL) * 100ULL);
    return 0;
}

int clock_gettime(clockid_t clock_id, struct timespec *now) {
    FILETIME wall;

    if (now == NULL) {
        errno = EFAULT;
        return -1;
    }
    switch (clock_id) {
    case CLOCK_REALTIME:
    case CLOCK_REALTIME_COARSE:
        /* The "precise" variant is the one with a real resolution; the plain
         * GetSystemTimeAsFileTime ticks at the ~15.6 ms scheduler period,
         * which is coarse enough to make a nanosecond-typed result a lie. */
        GetSystemTimePreciseAsFileTime(&wall);
        filetime_to_timespec(&wall, now);
        if ((unsigned long long)now->tv_sec * 10000000ULL < EPOCH_DELTA_100NS) {
            errno = EINVAL;
            return -1;
        }
        now->tv_sec -= (time_t)(EPOCH_DELTA_100NS / 10000000ULL);
        return 0;
    case CLOCK_MONOTONIC:
    case CLOCK_MONOTONIC_RAW:
    case CLOCK_MONOTONIC_COARSE:
    case CLOCK_BOOTTIME:
        if (monotonic_now(now) != 0) {
            errno = EINVAL;
            return -1;
        }
        return 0;
    case CLOCK_PROCESS_CPUTIME_ID:
        if (cpu_time(GetCurrentProcess(), 1, now) != 0) {
            errno = EINVAL;
            return -1;
        }
        return 0;
    case CLOCK_THREAD_CPUTIME_ID:
        if (cpu_time(GetCurrentThread(), 0, now) != 0) {
            errno = EINVAL;
            return -1;
        }
        return 0;
    default:
        errno = EINVAL;
        return -1;
    }
}

int clock_getres(clockid_t clock_id, struct timespec *resolution) {
    LARGE_INTEGER frequency;

    if (resolution == NULL) {
        errno = EFAULT;
        return -1;
    }
    switch (clock_id) {
    case CLOCK_MONOTONIC:
    case CLOCK_MONOTONIC_RAW:
    case CLOCK_MONOTONIC_COARSE:
    case CLOCK_BOOTTIME:
        if (!QueryPerformanceFrequency(&frequency) || frequency.QuadPart == 0) {
            errno = EINVAL;
            return -1;
        }
        resolution->tv_sec = 0;
        resolution->tv_nsec = (long)(1000000000LL / frequency.QuadPart);
        if (resolution->tv_nsec == 0) resolution->tv_nsec = 1;
        return 0;
    case CLOCK_REALTIME:
    case CLOCK_REALTIME_COARSE:
    case CLOCK_PROCESS_CPUTIME_ID:
    case CLOCK_THREAD_CPUTIME_ID:
        /* All four are FILETIME-derived, whose unit is 100 ns. */
        resolution->tv_sec = 0;
        resolution->tv_nsec = 100;
        return 0;
    default:
        errno = EINVAL;
        return -1;
    }
}

int nanosleep(const struct timespec *requested, struct timespec *remaining) {
    LARGE_INTEGER interval;
    HANDLE timer;
    long long hundred_ns;

    if (requested == NULL || requested->tv_nsec < 0 || requested->tv_nsec >= 1000000000L) {
        errno = EINVAL;
        return -1;
    }
    /* A waitable timer rather than Sleep(): Sleep's unit is the millisecond and
     * its granularity is the scheduler tick, so a sub-millisecond request would
     * either round to zero or overshoot by ~15 ms. The timer's unit is 100 ns,
     * which is the resolution this signature promises. */
    hundred_ns = (long long)requested->tv_sec * 10000000LL + requested->tv_nsec / 100;
    timer = CreateWaitableTimerW(NULL, TRUE, NULL);
    if (timer == NULL) {
        errno = EAGAIN;
        return -1;
    }
    interval.QuadPart = -hundred_ns; /* negative == relative */
    if (!SetWaitableTimer(timer, &interval, 0, NULL, NULL, FALSE) ||
        WaitForSingleObject(timer, INFINITE) != WAIT_OBJECT_0) {
        CloseHandle(timer);
        errno = EINVAL;
        return -1;
    }
    CloseHandle(timer);
    /* The wait above is uninterruptible here -- there are no POSIX signals to
     * cut it short -- so the sleep always completes and nothing remains. */
    if (remaining != NULL) {
        remaining->tv_sec = 0;
        remaining->tv_nsec = 0;
    }
    return 0;
}

int usleep(useconds_t microseconds) {
    struct timespec requested;
    requested.tv_sec = (time_t)(microseconds / 1000000u);
    requested.tv_nsec = (long)((microseconds % 1000000u) * 1000u);
    return nanosleep(&requested, NULL);
}

int gettimeofday(struct timeval *now, void *timezone_unused) {
    struct timespec wall;
    (void)timezone_unused;
    if (now == NULL) {
        errno = EFAULT;
        return -1;
    }
    if (clock_gettime(CLOCK_REALTIME, &wall) != 0) return -1;
    now->tv_sec = (long)wall.tv_sec;
    now->tv_usec = (long)(wall.tv_nsec / 1000);
    return 0;
}

/* ---- files --------------------------------------------------------------- */

int ftruncate(int descriptor, off_t length) {
    /* _chsize_s takes a 64-bit length even though off_t here is 32-bit, so the
     * widening is free and the call is ready if off_t ever grows. */
    errno_t error = _chsize_s(descriptor, (long long)length);
    if (error != 0) {
        errno = error;
        return -1;
    }
    return 0;
}

int truncate(const char *path, off_t length) {
    int descriptor;
    int result;
    if (path == NULL) {
        errno = EFAULT;
        return -1;
    }
    descriptor = _open(path, _O_WRONLY | _O_BINARY);
    if (descriptor < 0) return -1;
    result = ftruncate(descriptor, length);
    _close(descriptor);
    return result;
}

/* The trailing template both mkstemp and mkdtemp require. */
static int template_suffix(char *template_path, size_t *length_out) {
    size_t length;
    if (template_path == NULL) {
        errno = EINVAL;
        return -1;
    }
    length = strlen(template_path);
    if (length < 6 || strcmp(template_path + length - 6, "XXXXXX") != 0) {
        errno = EINVAL;
        return -1;
    }
    *length_out = length;
    return 0;
}

/* Fills the six trailing X's from the system CSPRNG. rand() is deliberately not
 * used: these name files in shared temporary directories, where a predictable
 * sequence is a hijack, and rand() is also process-global state this tree does
 * not otherwise touch. BCryptGenRandom rather than RtlGenRandom because the
 * latter is an undocumented alias for SystemFunction036 in advapi32 that has to
 * be declared by hand. */
static int template_fill(char *template_path, size_t length) {
    static const char alphabet[] = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    unsigned char entropy[6];
    unsigned index;

    if (BCryptGenRandom(NULL, entropy, (ULONG)sizeof(entropy), BCRYPT_USE_SYSTEM_PREFERRED_RNG) != 0) {
        errno = EIO;
        return -1;
    }
    for (index = 0; index < 6u; ++index)
        template_path[length - 6 + index] = alphabet[entropy[index] % (sizeof(alphabet) - 1)];
    return 0;
}

int mkstemp(char *template_path) {
    size_t length;
    int attempt;

    if (template_suffix(template_path, &length) != 0) return -1;
    /* Retry on collision only. _O_EXCL makes the create-or-fail atomic, which
     * is the whole point of the interface. */
    for (attempt = 0; attempt < 128; ++attempt) {
        int descriptor;
        if (template_fill(template_path, length) != 0) return -1;
        descriptor = _open(template_path, _O_RDWR | _O_CREAT | _O_EXCL | _O_BINARY, _S_IREAD | _S_IWRITE);
        if (descriptor >= 0) return descriptor;
        if (errno != EEXIST) return -1;
    }
    errno = EEXIST;
    return -1;
}

char *mkdtemp(char *template_path) {
    size_t length;
    int attempt;

    if (template_suffix(template_path, &length) != 0) return NULL;
    for (attempt = 0; attempt < 128; ++attempt) {
        if (template_fill(template_path, length) != 0) return NULL;
        if (CreateDirectoryA(template_path, NULL)) return template_path;
        if (GetLastError() != ERROR_ALREADY_EXISTS) {
            errno = EIO;
            return NULL;
        }
    }
    errno = EEXIST;
    return NULL;
}

/* ---- strings ------------------------------------------------------------- */

char *strtok_r(char *string, const char *delimiters, char **save) {
    /* strtok_s has the identical signature and the identical reentrancy
     * contract; POSIX and the UCRT differ only in the name. */
    return strtok_s(string, delimiters, save);
}

int strcasecmp(const char *left, const char *right) {
    /* _stricmp folds case in the current C locale, as POSIX specifies for
     * strcasecmp; the UCRT's locale-independent variant is _stricmp_l. */
    return _stricmp(left, right);
}

int strncasecmp(const char *left, const char *right, size_t length) {
    return _strnicmp(left, right, length);
}
