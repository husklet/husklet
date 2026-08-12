/*
 * <unistd.h> for the x86_64-pc-windows-msvc target.
 *
 * The UCRT has no such header, but it does have most of the contents under
 * other names: <io.h> carries read/write/close/lseek/access/dup/isatty (and,
 * with _CRT_DECLARE_NONSTDC_NAMES on, under exactly those spellings),
 * <direct.h> carries chdir/getcwd/rmdir, <process.h> carries getpid. So this
 * header is mostly a re-export, and the hand-written part is small enough to
 * list: the access() mode constants, the three standard descriptor numbers,
 * and the five calls the UCRT genuinely does not have.
 *
 * Those five -- ftruncate, truncate, usleep, mkstemp, mkdtemp -- are
 * implemented in posix.c beside this file. They are not stubs: each is a real
 * implementation over the UCRT or Win32, because the engine calls them on
 * paths that run.
 */

#ifndef HL_MSVC_POSIX_UNISTD_H
#define HL_MSVC_POSIX_UNISTD_H

#include <sys/types.h>

#include <direct.h>
#include <io.h>
#include <process.h>
#include <stddef.h>

/* access() modes. Windows has no execute bit, so X_OK is spelled as the value
 * POSIX gives it and _access() maps it onto a read check -- which is what
 * mingw-w64 does too, and what the UCRT's own _access does with mode 1. */
#ifndef F_OK
#define F_OK 0
#endif
#ifndef X_OK
#define X_OK 1
#endif
#ifndef W_OK
#define W_OK 2
#endif
#ifndef R_OK
#define R_OK 4
#endif

/* lseek() whences. The UCRT puts these in <stdio.h> only; mingw-w64's
 * <unistd.h> also carries them, and a call site that seeks a descriptor
 * reasonably includes <unistd.h> and not <stdio.h>. Same values in both. */
#ifndef SEEK_SET
#define SEEK_SET 0
#endif
#ifndef SEEK_CUR
#define SEEK_CUR 1
#endif
#ifndef SEEK_END
#define SEEK_END 2
#endif

#ifndef STDIN_FILENO
#define STDIN_FILENO 0
#endif
#ifndef STDOUT_FILENO
#define STDOUT_FILENO 1
#endif
#ifndef STDERR_FILENO
#define STDERR_FILENO 2
#endif

#ifdef __cplusplus
extern "C" {
#endif

int ftruncate(int descriptor, off_t length);
int truncate(const char *path, off_t length);
int usleep(useconds_t microseconds);
int mkstemp(char *template_path);
char *mkdtemp(char *template_path);

#ifdef __cplusplus
}
#endif

#endif /* HL_MSVC_POSIX_UNISTD_H */
