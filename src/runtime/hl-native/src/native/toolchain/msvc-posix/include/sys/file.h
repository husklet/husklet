/*
 * <sys/file.h> for the x86_64-pc-windows-msvc target.
 *
 * This header exists on Linux to carry flock() and its LOCK_* constants. It is
 * included from this tree's shared host code, but every flock() call site is
 * behind a non-Windows guard -- the Windows host locks through LockFileEx in
 * src/host/windows, which is a byte-range lock and not the same object.
 *
 * So the constants are declared and the function is NOT. Declaring flock()
 * would invite a future call site to link against something that does not
 * exist here, and a link error naming `flock` is a worse diagnostic than a
 * compile error naming it. The constants are harmless on their own and let a
 * shared TU that mentions LOCK_EX inside a dead branch still compile.
 */

#ifndef HL_MSVC_POSIX_SYS_FILE_H
#define HL_MSVC_POSIX_SYS_FILE_H

#define LOCK_SH 1
#define LOCK_EX 2
#define LOCK_NB 4
#define LOCK_UN 8

#endif /* HL_MSVC_POSIX_SYS_FILE_H */
