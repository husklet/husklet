/*
 * Force-included prelude for the x86_64-pc-windows-msvc target.
 *
 * Three of the missing POSIX names live in headers this directory deliberately
 * does NOT shim -- <limits.h>, <fcntl.h> and <time.h>. Those are hot, widely
 * included, and partly compiler-supplied (clang has its own <limits.h>), so
 * interposing on them to add one macro each would put this directory in the
 * path of every translation unit for no proportionate benefit. A prelude that
 * the toolchain file force-includes reaches the same TUs with none of that
 * reach.
 *
 * It is included before any other header, so it includes what it needs itself
 * rather than assuming an order.
 */

#ifndef HL_MSVC_PRELUDE_H
#define HL_MSVC_PRELUDE_H

/* POSIX spellings in <io.h>, <direct.h> and <process.h> -- open, read, write,
 * close, chdir, getpid. On by default in the UCRT, but stated rather than
 * assumed, because this whole seam is built on those spellings existing. */
#ifndef _CRT_DECLARE_NONSTDC_NAMES
#define _CRT_DECLARE_NONSTDC_NAMES 1
#endif
/* The UCRT deprecates its own POSIX aliases, and the string and memory
 * families, in favour of the bounds-checked _s variants. This tree is portable
 * C and will not be writing MSVC-only calls, so those warnings are pure noise
 * on a build that treats warnings as errors. */
#ifndef _CRT_SECURE_NO_WARNINGS
#define _CRT_SECURE_NO_WARNINGS 1
#endif
#ifndef _CRT_NONSTDC_NO_WARNINGS
#define _CRT_NONSTDC_NO_WARNINGS 1
#endif
#ifndef _WINSOCK_DEPRECATED_NO_WARNINGS
#define _WINSOCK_DEPRECATED_NO_WARNINGS 1
#endif

#include <fcntl.h>
#include <limits.h>
#include <stddef.h>
#include <time.h>

/* Windows has no sysconf namespace or Unix load averages. Keep these names
 * private to the compatibility seam so mingw libraries cannot accidentally
 * satisfy a different ABI spelling. */
#define sysconf hl_windows_sysconf
#define getloadavg hl_windows_getloadavg
#define arc4random_buf hl_windows_arc4random_buf
#ifndef _SC_CLK_TCK
#define _SC_CLK_TCK 1
#define _SC_OPEN_MAX 2
#define _SC_NPROCESSORS_ONLN 3
#endif

/* PATH_MAX. Not a Windows concept: the UCRT's nearest equivalent is _MAX_PATH
 * (260), the legacy limit, while the actual limit with a \\?\ prefix is 32767
 * UTF-16 units. This tree uses PATH_MAX to size buffers holding GUEST paths,
 * which are Linux paths, so the Linux value is the correct one -- a shorter
 * buffer would truncate a path the guest is entitled to use. mingw-w64 also
 * defines PATH_MAX as 260, and this is a deliberate divergence from it. */
#ifndef PATH_MAX
#define PATH_MAX 4096
#endif

/* The open() access-mode mask. The UCRT defines _O_RDONLY/_O_WRONLY/_O_RDWR as
 * 0/1/2 exactly as POSIX does, so the mask has its POSIX value; it simply has
 * no name in <fcntl.h> here. */
#ifndef O_ACCMODE
#define O_ACCMODE 0003
#endif

/* clock_gettime and its clock ids. C11's <time.h> in the UCRT has
 * timespec_get() and TIME_UTC but not the POSIX clock family. The ids are the
 * Linux values because this tree passes them across the guest ABI boundary in
 * places, and compatibility.c implements the four that are reachable. */
#ifndef CLOCK_REALTIME
#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
#define CLOCK_PROCESS_CPUTIME_ID 2
#define CLOCK_THREAD_CPUTIME_ID 3
#define CLOCK_MONOTONIC_RAW 4
#define CLOCK_REALTIME_COARSE 5
#define CLOCK_MONOTONIC_COARSE 6
#define CLOCK_BOOTTIME 7
typedef int clockid_t;
#endif

#ifdef __cplusplus
extern "C" {
#endif

int clock_gettime(clockid_t clock_id, struct timespec *now);
int clock_getres(clockid_t clock_id, struct timespec *resolution);
int nanosleep(const struct timespec *requested, struct timespec *remaining);
long hl_windows_sysconf(int name);
int hl_windows_getloadavg(double averages[], int count);
void hl_windows_arc4random_buf(void *buffer, size_t size);

/* Three string functions that mingw-w64 declares from <string.h> as GNU
 * extensions and the UCRT spells differently. The call sites include
 * <string.h> and not <strings.h>, which is where POSIX actually puts the
 * casecmp pair, so declaring them here reaches those TUs without this
 * directory having to interpose on <string.h> itself. Each has an exact UCRT
 * counterpart -- strtok_s, _stricmp, _strnicmp -- so compatibility.c forwards rather
 * than reimplementing. */
char *strtok_r(char *string, const char *delimiters, char **save);
/* mingw-w64 exposes these POSIX spellings as macros to the UCRT names.  This
 * layer provides real POSIX symbols, so keep both the declarations and their
 * compatibility.c definitions from being macro-renamed into recursive UCRT
 * replacements. */
#ifdef strcasecmp
#undef strcasecmp
#endif
#ifdef strncasecmp
#undef strncasecmp
#endif
int strcasecmp(const char *left, const char *right);
int strncasecmp(const char *left, const char *right, size_t length);

#ifdef __cplusplus
}
#endif

#endif /* HL_MSVC_PRELUDE_H */
