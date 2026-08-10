/* hl/host -- C-library spellings that differ between hosts.
 *
 * Nothing here is a feature decision. Every entry exists because two hosts offer
 * the SAME operation under a different name or arity, and the layers above must
 * carry neither spelling. If a host genuinely cannot perform an operation, that
 * is a feature guard at its own call site, not an entry here: a shim that
 * quietly does less than its POSIX namesake is exactly the debt this header is
 * meant to prevent.
 *
 * native_compat.h is the equivalent header for the host BACKENDS and is far
 * larger because it also emulates kernel objects. This one is included by
 * src/core and src/linux_abi, so it stays free of state, of host handles and of
 * <windows.h>.
 *
 * REQUIRES: an including TU must select a POSIX-visible libc -- the
 * `#define _POSIX_C_SOURCE 200809L` this tree already puts at the top of such
 * files. Under -std=c11 with no feature macro, glibc hides both mode_t and
 * unsetenv, and the definitions below will not compile. That is deliberately a
 * per-TU declaration rather than a #define here: a header that silently widens
 * the libc its includer sees changes every OTHER declaration in that TU too,
 * and does so depending on include order.
 */

#ifndef HL_HOST_LIBC_COMPAT_H
#define HL_HOST_LIBC_COMPAT_H

#include <stdlib.h>

#if defined(_WIN32)
#include <direct.h>
#else
#include <sys/stat.h>
#include <unistd.h>
#endif

/* mkdir(2) takes a creation mode. The UCRT's _mkdir takes only a path, and the
 * mkdir that mingw's <io.h> declares IS that one-argument function -- so a
 * two-argument call is a hard compile error rather than a silently ignored
 * argument. The mode is dropped rather than translated: Windows has no POSIX
 * permission bits to receive it and a new directory inherits its parent's ACL.
 * Callers that need a specific access control list must ask the host backend
 * for one; none of the current callers do. */
static inline int hl_compat_mkdir(const char *path, unsigned mode) {
#if defined(_WIN32)
    (void)mode;
    return _mkdir(path);
#else
    return mkdir(path, (mode_t)mode);
#endif
}

/* unsetenv(3) is POSIX-only; the UCRT offers no direct equivalent. Assigning a
 * variable the empty string removes its entry from the environment block
 * outright, so a later getenv() returns NULL exactly as it would after
 * unsetenv(). Returns 0 on success and -1 otherwise, matching POSIX. */
static inline int hl_compat_unsetenv(const char *name) {
#if defined(_WIN32)
    return _putenv_s(name, "") == 0 ? 0 : -1;
#else
    return unsetenv(name);
#endif
}

#endif
