/*
 * <sys/file.h> for the x86_64-pc-windows-msvc target.
 *
 * This header exists on Linux to carry flock() and its LOCK_* constants. Both
 * are provided here: the constants below, and a flock() implemented in
 * toolchain/msvc-posix/compatibility.c over LockFileEx/UnlockFileEx across the
 * whole byte range (offset 0, length MAXDWORD:MAXDWORD).
 *
 * That is NOT the same object as a Linux flock(2) lock, and a call site that
 * needs the difference must say so itself:
 *
 *   - Linux flock(2) locks the open file DESCRIPTION, so the lock is shared by
 *     dup()s and inherited across fork(); LockFileEx locks per HANDLE.
 *   - Linux allows a shared -> exclusive conversion in place; re-locking an
 *     already-locked range through LockFileEx does not convert it.
 *   - Linux flock(2) and fcntl(2) record locks are independent objects;
 *     LockFileEx is one byte-range lock space shared with everything else that
 *     locks the same file on this host.
 *
 * What it IS adequate for is the shape the shared host code actually uses:
 * serializing first-time creation or sizing of an engine-owned file among
 * separate processes, where a whole-range exclusive lock and a whole-range
 * release are the entire contract.
 *
 * A previous version of this comment said the function was deliberately NOT
 * declared, so that a Windows-reachable call site would fail to compile rather
 * than fail to link. That stopped being true in 3bb0e151e, which added both the
 * declaration below and the definition in compatibility.c, and the stale comment
 * then misled two readers into believing flock() is unavailable on this host.
 * It is available; whether a given POSIX-shaped mechanism SHOULD run on Windows
 * is a question for that mechanism, not for this header.
 */

#ifndef HL_MSVC_POSIX_SYS_FILE_H
#define HL_MSVC_POSIX_SYS_FILE_H

#define LOCK_SH 1
#define LOCK_EX 2
#define LOCK_NB 4
#define LOCK_UN 8

int flock(int descriptor, int operation);

#endif /* HL_MSVC_POSIX_SYS_FILE_H */
