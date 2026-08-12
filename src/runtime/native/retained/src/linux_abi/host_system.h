#ifndef HL_LINUX_ABI_HOST_SYSTEM_H
#define HL_LINUX_ABI_HOST_SYSTEM_H

/*
 * The residue: the handful of system-surface names this layer uses that belong
 * to no single POSIX header, and that the mingw-w64 CRT does not have.  Same
 * construction and the same REAL/SHAPE/REFUSAL labelling as host_mman.h and
 * host_poll.h.
 *
 * Everything here was found by compiling, not by predicting, which is why the
 * list is short and oddly assorted: <unistd.h>'s sysconf and the _SC_* it takes,
 * <sys/sysmacros.h>'s device-number split, <stdlib.h>'s arc4random_buf,
 * <stdio.h>'s two in-memory FILE constructors, <sys/types.h>'s suseconds_t,
 * <time.h>'s CLOCK_MONOTONIC_RAW, and fork(2).  Each one is here because a real
 * call site names it, and none of them justifies a header of its own.
 */

#if !defined(_WIN32)

#include <stdio.h>
#include <stdlib.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>
#if defined(__linux__)
#include <sys/sysmacros.h> /* major/minor moved out of <sys/types.h> in glibc 2.28 */
#endif

#else /* Windows */

#include "../host/process.h" /* the backend's clone/reap/kill bridge, for fork() below */

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

/* The CRT declares rand_s only when _CRT_RAND_S was defined BEFORE <stdlib.h>,
 * and by the time this header is reached <stdlib.h> has already been included
 * by something else in the unity TU -- so defining the macro here would be too
 * late and silently do nothing.  Declared by hand instead, which is the same
 * device src/host/native_compat.h uses for the Win32 entry points it calls, and
 * skipped when the macro DID reach <stdlib.h> first so the two declarations can
 * never drift apart. */
#ifndef _CRT_RAND_S
int __cdecl rand_s(unsigned int *value);
#endif

/* SHAPE.  suseconds_t is the signed microsecond count in struct timeval.  The
 * CRT has timeval but spells the member's type `long`, which is 32-bit under
 * LLP64 -- so this must be `long` too, or a cast through it truncates
 * differently from the assignment it feeds. */
typedef long suseconds_t;

/*
 * SHAPE.  CLOCK_MONOTONIC_RAW is Linux's un-slewed monotonic clock.  The CRT's
 * <time.h> declares CLOCK_REALTIME/MONOTONIC/PROCESS_CPUTIME_ID/
 * THREAD_CPUTIME_ID but not this one, and the single call site is a switch that
 * maps a GUEST clock id onto a typed host-seam clock -- so the value must be
 * Linux's 4, not an arbitrary spare number, because the guest supplies it.
 * Guarded so a future CRT that grows the macro wins instead of colliding.
 */
#ifndef CLOCK_MONOTONIC_RAW
#define CLOCK_MONOTONIC_RAW 4
#endif

/* REAL.  major/minor/makedev split a device number.  Pure arithmetic on a value
 * this layer itself composed, so a host with no device numbers withholds
 * nothing.  The encoding is Linux's 64-bit one -- major in bits 8..19 and
 * 32..63, minor in 0..7 and 20..31 -- because the numbers that flow through
 * here are the guest's, synthesized by this layer's own /proc and /dev
 * emulation, not the host's. */
static inline unsigned int major(uint64_t device) {
    return (unsigned int)(((device >> 8) & 0xfffu) | ((device >> 32) & ~0xfffu));
}

static inline unsigned int minor(uint64_t device) {
    return (unsigned int)((device & 0xffu) | ((device >> 12) & ~0xffu));
}

static inline uint64_t makedev(unsigned int high, unsigned int low) {
    return ((uint64_t)(high & 0xfffu) << 8) | ((uint64_t)(high & ~0xfffu) << 32) | ((uint64_t)(low & 0xffu)) |
           ((uint64_t)(low & ~0xffu) << 12);
}

/* SHAPE.  Only the _SC_* the call sites below actually pass. */
#define _SC_ARG_MAX 0
#define _SC_CLK_TCK 2
#define _SC_OPEN_MAX 4
#define _SC_PAGESIZE 30
#define _SC_PAGE_SIZE 30
#define _SC_NPROCESSORS_CONF 83
#define _SC_NPROCESSORS_ONLN 84

/*
 * REFUSAL, deliberately, and this one is worth being explicit about because two
 * of the three call sites would accept a plausible answer.
 *
 * _SC_CLK_TCK feeds the /proc/[pid]/stat and /proc/uptime jiffy conversion, and
 * _SC_NPROCESSORS_ONLN feeds an online-CPU count.  Both have obvious Windows
 * answers (a fixed 100, and GetSystemInfo's dwNumberOfProcessors).  They are
 * not given here for the same reason host_mman.h does not synthesize mprotect:
 * this header is included by the guest-target TU but must not become a second
 * host backend.  The engine already HAS a typed host system snapshot for these
 * quantities, and the honest fix is to route the call sites through it rather
 * than to answer them here from a different source that could disagree.
 *
 * -1 is sysconf's documented "no determinate limit / not supported" return, and
 * every call site in this layer already tests for it -- the _SC_NPROCESSORS_ONLN
 * site is itself a last-resort fallback with its own guard.
 */
static inline long sysconf(int name) {
    (void)name;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL.  A load average is a kernel-maintained decayed run-queue statistic;
 * Windows maintains no such number, and the processor-queue performance counter
 * that gets suggested for it is a different quantity sampled differently.
 * Returning 0.0 would tell a guest the machine is idle, which is a measurement,
 * not an absence -- so this returns -1 and the caller reports what it reports
 * for a host that cannot answer. */
static inline int getloadavg(double *averages, int count) {
    (void)averages;
    (void)count;
    return -1;
}

/*
 * REAL.  arc4random_buf fills a buffer with cryptographically strong bytes and
 * cannot fail.  RtlGenRandom (exported from advapi32 as SystemFunction036) is
 * the equivalent, but it is not used here: this header is pulled into the
 * guest-target TU and the import gate forbids widening that TU's import table.
 *
 * The bytes come from the CRT's rand_s instead, which is a thin wrapper over
 * the same RtlGenRandom the OS uses -- documented as suitable for cryptographic
 * use and seeded per call from the OS entropy pool, not from a PRNG state this
 * process could reproduce.  It reports failure, and there is no failure return
 * on this API, so a failed draw aborts rather than silently leaving a caller's
 * buffer at whatever it held: every call site here is seeding an ASLR base, a
 * stack canary or a guest getrandom(2), and each of those is a security
 * property that must not degrade quietly.
 */
static inline void arc4random_buf(void *buffer, size_t bytes) {
    unsigned char *out = (unsigned char *)buffer;
    size_t produced = 0;
    while (produced < bytes) {
        unsigned int word = 0;
        size_t chunk = bytes - produced;
        if (rand_s(&word) != 0) abort();
        if (chunk > sizeof(word)) chunk = sizeof(word);
        memcpy(out + produced, &word, chunk);
        produced += chunk;
    }
}

/* REAL.  fork(2) over the address-space clone this host does have.
 *
 * The clone is RtlCloneUserProcess and it is fork's shape exactly: a
 * copy-on-write duplicate of the whole address space at byte-identical
 * addresses, returning twice, carrying only the calling thread.  What it is NOT
 * is the process group's spawn -- that is a cold CreateProcess with no shared
 * address space, and it is a different call for a different purpose.
 *
 * Everything past the return is unchanged from the POSIX hosts: the child runs
 * the same fork-child repair hooks, because those hooks were written for the
 * only-the-calling-thread-survives rule and that rule is what a clone gives. */
static inline int fork(void) {
    return hl_host_windows_fork();
}

/* REFUSAL.  fmemopen and open_memstream produce a FILE * over memory. The CRT
 * has no way to construct one -- FILE is opaque and every constructor it
 * exports names a file, a descriptor or a pipe. NULL is the documented failure
 * and both call sites check it. */
static inline FILE *fmemopen(void *buffer, size_t size, const char *mode) {
    (void)buffer;
    (void)size;
    (void)mode;
    errno = ENOSYS;
    return NULL;
}

static inline FILE *open_memstream(char **buffer, size_t *size) {
    (void)buffer;
    (void)size;
    errno = ENOSYS;
    return NULL;
}

#endif /* _WIN32 */

#endif
