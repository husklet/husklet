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

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

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

#endif /* _WIN32 */

#endif
