#ifndef HL_LINUX_ABI_HOST_PROC_H
#define HL_LINUX_ABI_HOST_PROC_H

/*
 * <sys/resource.h>, <sys/utsname.h> and <sys/times.h> for this layer -- the
 * three headers that answer "what is this process allowed, what has it used,
 * and what is it running on".  Same construction and the same REAL/SHAPE/
 * REFUSAL labelling as host_mman.h and host_poll.h.
 *
 * Three headers in one seam because they are one question with one answer here.
 * Windows has each capability in some form -- job objects cap resources,
 * GetProcessTimes and GetProcessMemoryInfo report usage, GetVersionEx and
 * GetNativeSystemInfo describe the machine -- and NONE of them is reachable
 * from this file, which by construction includes no Windows header (host_poll.h
 * and native_context.h's Windows arm make the same choice, for the same reason:
 * pulling <windows.h> into a unity TU whose job is marshalling the GUEST's ABI
 * structures collides with them by name).  The route for all three is the typed
 * host seam, and the seam has no resource, accounting or system-identity
 * operation today.  So these are refusals with an obvious eventual fix, not
 * refusals with an argument behind them.
 *
 * WHAT IS NOT REFUSED, and why the distinction matters:
 *
 *   getrlimit/setrlimit/prlimit have NO caller in this layer at all, and that
 *   is not an accident.  The guest's limits are EMULATED end to end: they live
 *   in g_limits (container/state.c, seeded from docker --ulimit), the guest's
 *   own setrlimit/prlimit64 store into that table, and every enforcement point
 *   -- the RLIMIT_NOFILE gate in syscall/helpers.c, the RLIMIT_FSIZE gate on
 *   writes, the RLIMIT_CORE read behind WCOREDUMP -- reads it back.  The host's
 *   real limits are never consulted and never set.  The vocabulary is still
 *   spelled out below because the RLIMIT_* numbers are guest ABI (the guest
 *   passes them as syscall arguments and /proc/self/limits reports them in that
 *   order), so the names have to mean the Linux numbers even where no host call
 *   takes them.
 *
 *   The parts that DO have callers -- getrusage, times, getpriority,
 *   setpriority -- are the ones that read or write real host state, and those
 *   are the refusals that cost something.  Each is noted at its definition with
 *   what its caller does on failure, because in every case the caller was
 *   already written to survive it: the rusage buffers are zero-initialised
 *   before the call, and the priority pair is already best-effort by design.
 */

#if !defined(_WIN32)

#include <sys/resource.h>
#include <sys/times.h>
#include <sys/utsname.h>

#else /* Windows */

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/time.h> /* struct timeval, for struct rusage */
#include <sys/types.h>
#include <time.h> /* clock_t, for struct tms and times() */

/* SHAPE.  id_t -- the "a pid, a pgid or a uid, depending on the `which`
 * argument next to it" type.  <sys/types.h> supplies it on a POSIX host; the
 * CRT's does not.  It lands in this seam because the calls that take one are
 * here (getpriority/setpriority) and in host_wait.h (waitid), and one of the
 * two had to own it.  Unsigned 32-bit, as on Linux. */
typedef unsigned int id_t;

/* ---- SHAPE: rlimit vocabulary.  Linux values. ---------------------------
 * rlim_t is 64-bit unconditionally, matching Linux's __rlim64_t and the
 * prlimit64 ABI the guest actually uses -- not a `long`, which would be 32 bits
 * on this host and would make RLIM_INFINITY a different number from the one the
 * guest sends. */
typedef uint64_t rlim_t;
typedef uint64_t rlim64_t;

#define RLIM_INFINITY ((rlim_t)~0ULL)
#define RLIM64_INFINITY RLIM_INFINITY
#define RLIM_SAVED_MAX RLIM_INFINITY
#define RLIM_SAVED_CUR RLIM_INFINITY

#define RLIMIT_CPU 0
#define RLIMIT_FSIZE 1
#define RLIMIT_DATA 2
#define RLIMIT_STACK 3
#define RLIMIT_CORE 4
#define RLIMIT_RSS 5
#define RLIMIT_NPROC 6
#define RLIMIT_NOFILE 7
#define RLIMIT_MEMLOCK 8
#define RLIMIT_AS 9
#define RLIMIT_LOCKS 10
#define RLIMIT_SIGPENDING 11
#define RLIMIT_MSGQUEUE 12
#define RLIMIT_NICE 13
#define RLIMIT_RTPRIO 14
#define RLIMIT_RTTIME 15
#define RLIM_NLIMITS 16

struct rlimit {
    rlim_t rlim_cur;
    rlim_t rlim_max;
};

struct rlimit64 {
    rlim64_t rlim_cur;
    rlim64_t rlim_max;
};

/*
 * SHAPE.  struct rusage, Linux's full field set in Linux's order.
 *
 * All sixteen fields, not just the ten syscall/proc.c's rusage_to_linux()
 * currently reads, because this struct's whole purpose at that call site is to
 * be the SOURCE of a field-by-field translation into the guest's 144-byte
 * layout -- a shortened host struct would silently narrow what that function
 * could ever grow to copy.  `long` is the right type for the counters even at
 * 32 bits here: they are counts, the translation widens each to int64_t on the
 * way out, and nothing on this host ever fills them.
 */
struct rusage {
    struct timeval ru_utime;
    struct timeval ru_stime;
    long ru_maxrss;
    long ru_ixrss;
    long ru_idrss;
    long ru_isrss;
    long ru_minflt;
    long ru_majflt;
    long ru_nswap;
    long ru_inblock;
    long ru_oublock;
    long ru_msgsnd;
    long ru_msgrcv;
    long ru_nsignals;
    long ru_nvcsw;
    long ru_nivcsw;
};

#define RUSAGE_SELF 0
#define RUSAGE_CHILDREN (-1)
#define RUSAGE_THREAD 1

/* ---- SHAPE: priority vocabulary.  Linux values. -------------------------
 * PRIO_MIN/PRIO_MAX are Linux's -20..20 and NOT this host's, which matters:
 * syscall/proc.c clamps a guest setpriority into [-20, 19] BEFORE the host call
 * precisely because a host with a different PRIO_MAX let nice settle one off
 * the Linux ceiling.  Getting the constants from the guest's ABI rather than
 * the host's is what makes that clamp mean the same thing everywhere. */
#define PRIO_PROCESS 0
#define PRIO_PGRP 1
#define PRIO_USER 2
#define PRIO_MIN (-20)
#define PRIO_MAX 20

/* ---- SHAPE: uname.  Linux's struct, including the GNU domainname field. --
 * Each field is char[65] -- 64 characters plus the terminator, Linux's
 * _UTSNAME_LENGTH -- and NOT the 256-byte fields some other Unixes use.  The
 * guest reads this layout byte for byte out of its own uname(2) buffer, so the
 * size is ABI rather than a buffer choice. */
#define _UTSNAME_LENGTH 65

struct utsname {
    char sysname[65];
    char nodename[65];
    char release[65];
    char version[65];
    char machine[65];
    char domainname[65];
};

/* ---- SHAPE: times().  Linux's struct tms. ------------------------------- */
struct tms {
    clock_t tms_utime;
    clock_t tms_stime;
    clock_t tms_cutime;
    clock_t tms_cstime;
};

/* ---- REFUSAL: the rlimit calls. ----------------------------------------
 * No caller in this layer today (see the header note -- the guest's limits are
 * emulated in g_limits and never round-trip through the host).  Present so the
 * vocabulary above has the calls that go with it, and refusing rather than
 * answering RLIM_INFINITY: a fabricated "unlimited" is the single answer most
 * likely to be believed and acted on, and it is the one that would silently
 * disagree with the /proc/self/limits text this layer generates from g_limits. */
static inline int getrlimit(int resource, struct rlimit *limits) {
    (void)resource;
    (void)limits;
    errno = ENOSYS;
    return -1;
}

static inline int setrlimit(int resource, const struct rlimit *limits) {
    (void)resource;
    (void)limits;
    errno = ENOSYS;
    return -1;
}

static inline int prlimit(pid_t pid, int resource, const struct rlimit *new_limit, struct rlimit *old_limit) {
    (void)pid;
    (void)resource;
    (void)new_limit;
    (void)old_limit;
    errno = ENOSYS;
    return -1;
}

static inline int prlimit64(pid_t pid, int resource, const struct rlimit64 *new_limit, struct rlimit64 *old_limit) {
    (void)pid;
    (void)resource;
    (void)new_limit;
    (void)old_limit;
    errno = ENOSYS;
    return -1;
}

/*
 * REFUSAL.  Per-process CPU and fault accounting.  Windows has the numbers --
 * GetProcessTimes for the two timevals, GetProcessMemoryInfo for the peak
 * working set behind ru_maxrss -- but no Windows header may be reached from
 * here, and the typed host seam has no accounting operation to route through.
 *
 * The three callers survive it exactly: syscall/proc.c's getrusage and
 * waitid's rusage arm both declare `uint8_t linux_ru[144] = {0}` and only
 * overwrite it when this returns 0, so the guest gets a well-formed all-zero
 * rusage rather than the sentinel garbage an unwritten buffer would expose.
 * Zeroes are a plausible reading for a process that has used nothing; a
 * fabricated one would not be, which is why the fabrication belongs at the
 * caller's initialiser and not in this function's return value.
 */
static inline int getrusage(int who, struct rusage *usage) {
    (void)who;
    (void)usage;
    errno = ENOSYS;
    return -1;
}

/*
 * REFUSAL.  times(2) reports the process's own and its reaped children's user
 * and system CPU in USER_HZ ticks.  Same absence as getrusage, and the return
 * value carries its own trap: times() returns a CLOCK, not a status, and its
 * documented failure value is (clock_t)-1 with errno set.  Returning 0 here
 * would be a legal-looking tick count.
 *
 * syscall/time.c's case 153 passes the result straight back to the guest, which
 * therefore sees -1 -- the same thing a Linux guest sees from a failed times().
 */
static inline clock_t times(struct tms *buffer) {
    (void)buffer;
    errno = ENOSYS;
    return (clock_t)-1;
}

/*
 * REFUSAL.  Scheduling nice values.  Windows has priority classes and thread
 * priorities, but they are a different model (class x relative level, not a
 * single signed nice), and mapping the two is a policy decision rather than a
 * translation -- there is no correct answer to "which class is nice 7".
 *
 * getpriority's caller in syscall/proc.c is written for exactly this: it clears
 * errno, calls, and treats (-1 with errno set) as failure -> ESRCH to the
 * guest, because -1 is also a legal nice value.  setpriority's caller ignores
 * the result on purpose ("the priority set itself stays best-effort success"),
 * so a guest that lowers its nice sees success and no change -- which is what a
 * host that declines the request already produced there.
 */
static inline int getpriority(int which, id_t who) {
    (void)which;
    (void)who;
    errno = ENOSYS;
    return -1;
}

static inline int setpriority(int which, id_t who, int priority) {
    (void)which;
    (void)who;
    (void)priority;
    errno = ENOSYS;
    return -1;
}

/*
 * REFUSAL.  uname(2) on the HOST.  Note carefully what this is not: the guest's
 * uname is synthesized by this layer from the container's configured kernel
 * identity and never comes from here, which is why no call site exists.  A
 * Windows host filling sysname="Windows_NT" would be a true answer to a
 * question nothing asks, and a wrong one if anything ever did -- every consumer
 * of a uname in this tree wants the GUEST's Linux identity.
 */
static inline int uname(struct utsname *name) {
    (void)name;
    errno = ENOSYS;
    return -1;
}

#endif /* _WIN32 */

#endif
