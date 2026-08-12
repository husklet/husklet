/*
 * <sys/time.h> for the x86_64-pc-windows-msvc target.
 *
 * struct timeval is the only thing this tree wants from it, and Windows does
 * have the type -- in <winsock2.h>, because it is select()'s timeout argument.
 * Reaching it that way is refused here: pulling Winsock into every translation
 * unit that wants a microsecond pair would put the whole Win32 socket
 * vocabulary into the guest-ABI TUs, which src/host/windows/win32.h goes to
 * some length to avoid.
 *
 * So the structure is declared directly. The field types match Winsock's
 * (`long`, `long`), not Linux's (time_t, suseconds_t): a struct timeval that
 * crosses into a Winsock call has to be the one Winsock expects.
 *
 * The duplicate-definition guard is awkward and worth stating. mingw-w64
 * guards its copy with _TIMEVAL_DEFINED and its <winsock2.h> honours that
 * macro, so the two agree whichever arrives first. The Windows SDK's
 * <winsock.h> does not: its `struct timeval` is unguarded. So the test below
 * is on _WINSOCKAPI_ / _WINSOCK2API_ -- the include guards of the two Winsock
 * headers -- which handles "Winsock first, then this header". The reverse
 * order cannot be handled from here at all, and does not arise: the one file
 * in this tree that includes <windows.h>, src/host/windows/win32.h, sets
 * WIN32_LEAN_AND_MEAN, which is precisely the switch that excludes
 * <winsock.h>. A future TU that wants both must include the Winsock header
 * first.
 */

#ifndef HL_MSVC_POSIX_SYS_TIME_H
#define HL_MSVC_POSIX_SYS_TIME_H

#include <sys/types.h>
#include <time.h>

#if !defined(_TIMEVAL_DEFINED) && !defined(_WINSOCKAPI_) && !defined(_WINSOCK2API_)
#define _TIMEVAL_DEFINED
struct timeval {
    long tv_sec;
    long tv_usec;
};
#endif

#ifndef _TIMEZONE_DEFINED
#define _TIMEZONE_DEFINED
struct timezone {
    int tz_minuteswest;
    int tz_dsttime;
};
#endif

#define timerisset(tvp) ((tvp)->tv_sec || (tvp)->tv_usec)
#define timerclear(tvp) ((tvp)->tv_sec = (tvp)->tv_usec = 0)

#ifdef __cplusplus
extern "C" {
#endif

int gettimeofday(struct timeval *now, void *timezone_unused);

#ifdef __cplusplus
}
#endif

#endif /* HL_MSVC_POSIX_SYS_TIME_H */
