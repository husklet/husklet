/*
 * <sys/types.h> for the x86_64-pc-windows-msvc target.
 *
 * This directory exists for one reason. The engine's Windows lane is GNU C
 * compiled by mingw-w64 clang, and mingw-w64 ships a POSIX seam -- <unistd.h>,
 * <pthread.h>, ssize_t, S_ISDIR -- that the MSVC target does not. The MSVC ABI
 * is a separate question from the POSIX surface, and clang emits MSVC-ABI COFF
 * perfectly well; what it cannot do is invent the headers. So the seam is
 * supplied here, ONE header per POSIX header the archive's source closure
 * actually includes, and nothing speculative.
 *
 * Every file here forwards to the real UCRT header with #include_next and adds
 * only what is missing. That ordering is load-bearing: this directory is passed
 * with -isystem, so it is searched BEFORE the UCRT include roots, and
 * #include_next resumes the search after it. Copying UCRT declarations instead
 * would drift the moment the SDK version changes.
 *
 * The contents are decided by one rule, not by taste: this file declares
 * exactly what mingw-w64's <sys/types.h> declares and the UCRT's does not.
 * Anything narrower fails to compile; anything wider collides.
 *
 * Two collisions are worth naming, because both were real:
 *
 *   off_t, ino_t, dev_t -- the UCRT already typedefs all three (guarded by
 *     _CRT_INTERNAL_NONSTDC_NAMES, on by default), so adding them produced
 *     "typedef redefinition with different types": the UCRT's off_t is `long`,
 *     not int64_t. Leaving them alone also keeps the widths identical to
 *     mingw-w64's, which likewise leaves off_t `long` unless _FILE_OFFSET_BITS
 *     is 64, and this tree never sets that. Widening off_t here would make the
 *     two Windows archives disagree about a struct-stat field width, which is
 *     the class of silent mismatch that having a separate ABI is meant to
 *     avoid.
 *
 *   uid_t, gid_t, nlink_t, blkcnt_t, blksize_t -- mingw-w64 does not declare
 *     these either, so src/linux_abi/host_fd.h already declares them itself
 *     for the Windows arm, at Linux widths, because they cross the guest ABI.
 *     They are that file's to own on BOTH Windows targets. Declaring them here
 *     as well collided on nlink_t (`unsigned short` against its `unsigned
 *     int`) and would have made this directory a second, disagreeing authority
 *     on a guest-visible width.
 */

#ifndef HL_MSVC_POSIX_SYS_TYPES_H
#define HL_MSVC_POSIX_SYS_TYPES_H

#include_next <sys/types.h>

#include <stddef.h>
#include <stdint.h>

#ifndef _SSIZE_T_DEFINED
#define _SSIZE_T_DEFINED
typedef intptr_t ssize_t;
#endif

/* Widths taken from mingw-w64's <sys/types.h>, member for member. */
#ifndef _PID_T_
#define _PID_T_
typedef int pid_t;
#endif

#ifndef _MODE_T_
#define _MODE_T_
typedef unsigned short mode_t;
#endif

#ifndef HL_MSVC_POSIX_USECONDS_T
#define HL_MSVC_POSIX_USECONDS_T
typedef unsigned int useconds_t;
#endif

#endif /* HL_MSVC_POSIX_SYS_TYPES_H */
