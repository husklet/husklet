#ifndef HL_LINUX_ABI_HOST_FD_H
#define HL_LINUX_ABI_HOST_FD_H

/*
 * <fcntl.h>, <sys/file.h> and the DESCRIPTOR half of <unistd.h> for this layer.
 *
 * Same construction and the same REAL/SHAPE/REFUSAL labelling as host_mman.h;
 * see that file's header for why this vocabulary lives in src/linux_abi rather
 * than in src/host/native_compat.h.  native_compat.h's Windows arm says the same
 * thing from the other side, by name: "Deliberately NOT here: the POSIX
 * descriptor vocabulary (fcntl/F_*, openat and the *at family, AT_FDCWD,
 * O_CLOEXEC/O_DIRECTORY ...)".  This file is where it went.
 *
 * ONE FACT DECIDES EVERY LABEL BELOW, and it is the opposite of the fact that
 * decides host_mman.h and host_uio.h.  Those two refuse almost everything
 * because a guest fd number names no host object on Windows.  That is not true
 * here.  The UCRT has a real descriptor table -- _open/_read/_write/_close/
 * _lseek/_dup/_dup2/_pipe/_commit all operate on int fds, the table holds 8192
 * of them (measured: the loop stops at fd 8191 with EMFILE), and the fds this
 * layer hands the guest ARE those descriptors.  So ordinary open, read, write,
 * close, lseek, dup, dup2, access, chdir, getcwd, unlink, rmdir and ftruncate
 * are already REAL on this host and this header does not touch them: a guest
 * that only issues write(2) and exit_group(2) runs on mingw's own declarations.
 *
 * What is missing is therefore not "the descriptor", it is a specific and
 * enumerable set of POSIX operations ON a descriptor.  The refusals below fall
 * into exactly three populations:
 *
 *   (1) THE UCRT HAS NO DIRECTORY DESCRIPTORS.  Measured: open(".", O_RDONLY)
 *       fails EACCES -- the low-I/O layer will not produce an fd for a
 *       directory at all.  Every *at() call therefore has no dirfd to resolve
 *       against.  The AT_FDCWD case is the exception and it is not an
 *       approximation of anything: openat(AT_FDCWD, p, ...) IS open(p, ...),
 *       so those forward and are REAL.  A real dirfd is refused.  Synthesizing
 *       one from getcwd()+chdir() was rejected for the reason host_uio.h
 *       rejects a writev loop: the *at() family exists to be atomic against a
 *       concurrent rename of the parent, and a chdir sandwich is neither
 *       atomic nor thread-safe -- a racing thread would resolve its own paths
 *       against someone else's cwd.
 *
 *   (2) THE UCRT EXPOSES NO PER-DESCRIPTOR METADATA CHANNEL.  fcntl(2)'s whole
 *       job is querying and altering state the descriptor carries -- FD_CLOEXEC,
 *       the O_* status flags, POSIX record locks, the SIGIO owner -- and the
 *       CRT publishes none of it.  Every one of those answers lives on the
 *       underlying HANDLE (GetHandleInformation, LockFileEx) and reaching it
 *       means <windows.h> and a descriptor->HANDLE binding, which a header
 *       pulled into the guest-target unity TU must not do.  fcntl is refused
 *       whole rather than answering some commands and not others, because a
 *       partial fcntl is indistinguishable at the call site from a complete one.
 *
 *   (3) NO PRIMITIVE EXISTS ANYWHERE IN THE CRT.  Symlinks and hard links,
 *       positional read/write, nanosecond timestamps, uids and gids, process
 *       groups and sessions, kill(2), whole-system sync.  Some of these have a
 *       Win32 answer the BACKEND can give (CreateSymbolicLinkW, ReadFile with
 *       an OVERLAPPED offset, the token's SIDs); none has one the CRT gives.
 *
 * THE FLAG VALUES, which is the one place this header deliberately does NOT use
 * Linux numbers.  mingw already defines O_CREAT, O_EXCL, O_TRUNC and O_APPEND,
 * and only one of the four agrees with Linux:
 *
 *          Linux    mingw          mingw's value is Linux's ...
 *   O_CREAT  0x40   0x100          O_NOCTTY
 *   O_EXCL   0x80   0x400          O_APPEND
 *   O_TRUNC  0x200  0x200          (agree)
 *   O_APPEND 0x400  0x8            --
 *
 * They are NOT redefined, and the call sites are why.  Unlike the PROT_/MAP_
 * words in host_mman.h and the termios words in host_tty.h, this layer never
 * passes a guest open-flag word into a host open().  syscall/fs.c builds the
 * host word bit by bit from NUMERIC guest constants -- `if (lf & 0x40) mf |=
 * O_CREAT; if (lf & 0x80) mf |= O_EXCL; ...` -- and syscall/io.c reverses it for
 * F_GETFL.  The translation is explicit in both directions, so the host macro
 * must carry whatever value the HOST's open() understands, and forcing Linux
 * numbers on them would be actively wrong: Linux's O_NOCTTY (0x100) is mingw's
 * O_CREAT, so `mf | O_NOCTTY` on fs.c's ptsname path would have started
 * creating files.
 *
 * The flags mingw LACKS are given bits at 1 << 20 and above.  Measured: the
 * UCRT's _open silently ignores flag bits it does not know (0x800, 1<<30 and
 * 1<<31 all opened normally, no EINVAL), and _O_U8TEXT at 0x40000 is the
 * highest bit it does know -- so bits from 1<<20 up can never alias a UCRT
 * flag, and a stray one can never turn an open into something else.  Silently
 * ignored is still silently ignored, though, so openat() below REFUSES a flag
 * word carrying a bit the CRT cannot act on rather than dropping it.  The one
 * that is not ignored is O_CLOEXEC: _O_NOINHERIT is exactly close-on-exec (the
 * CRT marks the HANDLE non-inheritable and leaves the fd out of the inherited
 * descriptor blob), so O_CLOEXEC is that flag and is REAL.
 *
 * AT_*, F_*, LOCK_*, UTIME_* and FD_CLOEXEC all keep their LINUX values, for two
 * different reasons.  LOCK_SH/EX/NB/UN and UTIME_NOW/UTIME_OMIT are compared
 * against words that came FROM THE GUEST (syscall/helpers.c's hl_flock takes the
 * guest's flock(2) operation directly; syscall/fs.c's utimensat path tests the
 * guest's tv_nsec), so identity there is required, exactly as it is for the
 * poll bits.  F_* and AT_* only ever reach the refusals in this file, so their
 * values are pure SHAPE and the Linux ones are simply the self-documenting
 * choice -- with AT_FDCWD at -100 matching HL_LINUX_AT_FDCWD so that
 * dispatch.c's ATFD() remap stays an identity.
 *
 * TWO RESIDUALS THIS HEADER CANNOT REACH, named here rather than hidden:
 *
 *   - The UCRT opens files in TEXT mode by default (measured: write(fd,"a\nb\n",4)
 *     lands 6 bytes on disk).  POSIX open has no newline translation, so every
 *     open THIS FILE performs forces _O_BINARY.  The bare open()/read()/write()
 *     call sites elsewhere in the layer keep the CRT default, and cannot be
 *     corrected from here: `open` is a real mingw declaration, so it cannot be
 *     shadowed by a static inline, and a function-like macro would also rewrite
 *     the `->open(` / `->read(` / `->write(` seam-vtable calls in this tree.
 *     The fix is the Windows host backend's file group, which already takes
 *     typed HL_HOST_FILE_* access words instead of a flag bit-set.
 *
 *   - RESOLVED. src/linux_abi/errno.c's hl_linux_errno_from_macos() now has a
 *     Windows arm.  The residual noted here was worse than recorded: the
 *     function is not the identity off Apple, it fed the UCRT's number to the
 *     DARWIN table, so a refusal's ENOSYS (UCRT 40) reached the guest as Linux
 *     90 EMSGSIZE -- "message too long" for "not implemented" -- rather than as
 *     the ELOOP predicted here.  Both are wrong; it is now 38.
 */

/*
 * The HOST's spelling of the null device, for the several places in this layer
 * that open it to MINT A DESCRIPTOR rather than to read or write anything --
 * the mq_open broker's queue handle, the pidfd fallback, the bound-shadow
 * sentinel, the checkpoint placeholders.  Each of those needs a real, closeable,
 * pollable descriptor that owns no data, and the null device is the cheapest one
 * on every host.
 *
 * It is here rather than at each call site because it is not the same string
 * everywhere: Windows' null device is the reserved DOS name NUL, and it is not
 * reachable as "/dev/null" from the CRT at all -- an open of that path is an
 * ordinary ENOENT.  That single missing name is what made every one of those
 * call sites fail on this host, and none of them is about /dev being absent.
 *
 * NOT for the guest-visible /dev/null, which is a different question with a
 * different answer: container/vfs.c maps the guest's path and must keep saying
 * "/dev/null", because that is the name the GUEST is asking about.
 */
#if !defined(_WIN32)
#define HL_LINUX_HOST_NULL_DEVICE "/dev/null"
#else
#define HL_LINUX_HOST_NULL_DEVICE "NUL"
#endif

#if !defined(_WIN32)

#include <fcntl.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>

#else /* Windows */

#include "../host/process.h" /* the backend's process bridge, for getppid() below */
#include "fdhandle.h"        /* the descriptor -> host-handle table this file's REALs go through */

#include <direct.h> /* _mkdir: the UCRT puts it here, not in <io.h> */
#include <errno.h>
#include <fcntl.h>
#include <io.h>
#include <limits.h>
#include <stdarg.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <time.h>
#include <unistd.h>

/* ---- SHAPE: the POSIX id/count types the UCRT's <sys/types.h> omits. -------
 * Linux widths.  uid_t/gid_t are unsigned there and the guest ABI marshals them
 * as uint32; the UCRT's own struct stat carries st_uid/st_gid as short, so a
 * comparison against one of these widens rather than truncates. */
typedef unsigned int uid_t;
typedef unsigned int gid_t;
typedef unsigned int nlink_t;
typedef long long blkcnt_t;
typedef long blksize_t;

/* SHAPE.  Linux's EDQUOT, produced by syscall/binding.c when the host seam
 * answers HL_STATUS_QUOTA -- a GUEST errno, so the Linux number is the correct
 * one.  It collides numerically with the UCRT's own ENOMSG (also 122); harmless
 * here because nothing maps 122 back, and see the errno residual above. */
#ifndef EDQUOT
#define EDQUOT 122
#endif

/* ---- SHAPE: open flags the UCRT lacks. -----------------------------------
 * Bits at 1 << 20 and above, out of reach of every flag _open knows (highest:
 * _O_U8TEXT, 0x40000).  See the header note for why these are not the Linux
 * values.  Guarded so a host header that grows one of them later wins. */
#ifndef O_NONBLOCK
#define O_NONBLOCK 0x00100000
#endif
#ifndef O_NOCTTY
#define O_NOCTTY 0x00200000
#endif
#ifndef O_DIRECTORY
#define O_DIRECTORY 0x00400000
#endif
#ifndef O_NOFOLLOW
#define O_NOFOLLOW 0x00800000
#endif
#ifndef O_ASYNC
#define O_ASYNC 0x01000000
#endif
#ifndef O_PATH
#define O_PATH 0x02000000
#endif
#ifndef O_SYNC
#define O_SYNC 0x04000000
#endif
#ifndef O_DSYNC
#define O_DSYNC 0x08000000
#endif

/* SHAPE.  Not a Windows flag and not a Linux one either: O_SYMLINK is Darwin's
 * "open the link node itself".  native_compat.h's LINUX arm synthesizes it from
 * O_PATH|O_NOFOLLOW for the container-vfs open path and its Windows arm does
 * not, so the same definition belongs here.  Both halves are refused by
 * openat() below, which is the honest answer: the CRT cannot name a link node. */
#ifndef O_SYMLINK
#define O_SYMLINK (O_PATH | O_NOFOLLOW)
#endif

/* REAL.  _O_NOINHERIT is close-on-exec, not an approximation of it. */
#ifndef O_CLOEXEC
#define O_CLOEXEC _O_NOINHERIT
#endif

/* The flag bits the UCRT's _open can actually act upon.  Anything outside this
 * mask would be accepted and ignored, so openat() refuses instead.  O_NOCTTY is
 * inside it and is the one vacuous member: nothing on Windows acquires a
 * controlling terminal, so "do not acquire one" is already true. */
#define HL_LINUX_HOST_OPEN_ACTIONABLE                                                                                  \
    (O_ACCMODE | O_APPEND | O_CREAT | O_TRUNC | O_EXCL | O_CLOEXEC | O_NOCTTY | O_BINARY | O_TEXT | O_TEMPORARY |      \
     O_RANDOM | O_SEQUENTIAL)

/* ---- SHAPE: fcntl commands and the descriptor flag.  Linux values. -------- */
#define F_DUPFD 0
#define F_GETFD 1
#define F_SETFD 2
#define F_GETFL 3
#define F_SETFL 4
#define F_GETLK 5
#define F_SETLK 6
#define F_SETLKW 7
#define F_SETOWN 8
#define F_GETOWN 9
#define F_SETSIG 10
#define F_GETSIG 11
#define F_DUPFD_CLOEXEC 1030

#define F_RDLCK 0
#define F_WRLCK 1
#define F_UNLCK 2

#define FD_CLOEXEC 1

/* SHAPE.  Reaches nothing but the fcntl() refusal below, so the layout is the
 * Linux one for readability rather than for compatibility; the GUEST's flock
 * image is decoded separately by the syscall layer and does not come through
 * this type.  l_start/l_len take the host off_t (32-bit on this CRT) so that a
 * caller's `struct flock` initializer compiles unchanged. */
struct flock {
    short l_type;
    short l_whence;
    off_t l_start;
    off_t l_len;
    pid_t l_pid;
};

/* ---- SHAPE: *at() directory-fd sentinel and flags.  Linux values. --------- */
#define AT_FDCWD (-100)
#define AT_SYMLINK_NOFOLLOW 0x100
#define AT_EACCESS 0x200
#define AT_REMOVEDIR 0x200
#define AT_SYMLINK_FOLLOW 0x400
#define AT_NO_AUTOMOUNT 0x800
#define AT_EMPTY_PATH 0x1000

/* ---- SHAPE: flock(2) operations.  Linux values, and here identity MATTERS --
 * syscall/helpers.c's hl_flock() receives the guest's operation word and tests
 * it against these names directly. */
#define LOCK_SH 1
#define LOCK_EX 2
#define LOCK_NB 4
#define LOCK_UN 8

/* ---- SHAPE: utimensat sentinels.  Linux values; compared against a timespec
 * the GUEST supplied, so identity matters here too. ------------------------- */
#define UTIME_NOW ((1L << 30) - 1)
#define UTIME_OMIT ((1L << 30) - 2)

/*
 * REAL.  The UCRT's low-I/O descriptor table is a fixed 8192 entries and has no
 * query, so this reports the measured ceiling rather than a guess: opening in a
 * loop stops at fd 8191 with EMFILE.  POSIX getdtablesize() cannot fail, and
 * this cannot either.  Its one caller (syscall/proc.c's exec CLOEXEC sweep
 * fallback) uses it as a scan bound, which is exactly what the ceiling is.
 */
static inline int getdtablesize(void) {
    return 8192;
}

/*
 * REAL.  _commit() is FlushFileBuffers on the descriptor's handle -- the same
 * operation fsync(2) names, with the same durability guarantee.
 */
static inline int fsync(int descriptor) {
    return _commit(descriptor);
}

/*
 * REAL.  _pipe() is the anonymous pipe, and the two arguments POSIX pipe(2) does
 * not carry are both pinned to the right thing here: 65536 is Linux's default
 * pipe capacity, and _O_BINARY is mandatory -- the CRT's default is text mode
 * (see the header note), and newline translation inside a pipe would corrupt
 * every byte stream the guest pushes through one.
 */
static inline int pipe(int descriptors[2]) {
    return _pipe(descriptors, 65536, _O_BINARY);
}

/*
 * REAL, with one named residual.  The UCRT has no nofollow stat and no
 * S_IFLNK -- native_compat.h's Windows arm says so where it defines S_ISLNK,
 * and reports a reparse point as a regular file.  So lstat() here IS stat():
 * for everything the CRT can see the two questions have the same answer, and
 * for a symlink this reports the target.  That is the residual, and it is the
 * same one every S_ISLNK() on this host already carries; refusing instead would
 * make lstat() wrong for ordinary files too, which is strictly worse.
 */
static inline int lstat(const char *path, struct stat *status) {
    return stat(path, status);
}

/*
 * REAL, with one named residual.  _fullpath() canonicalizes . and .. and makes
 * the path absolute, which is realpath(3)'s job minus symlink resolution -- and
 * the CRT cannot resolve a symlink anyway (same residual as lstat above).  Two
 * corrections are needed to make it realpath rather than "a path-shaped string":
 * _fullpath answers for a path that does not exist (measured: it happily folded
 * "nope/../zzz.txt"), where realpath must fail ENOENT; and it must never
 * overrun a caller's buffer, whose POSIX-contracted size is PATH_MAX.  Both are
 * applied below, so a NULL return here means the same thing it means on Linux.
 */
static inline char *realpath(const char *path, char *resolved) {
    char *canonical = _fullpath(NULL, path, 0);
    struct stat status;
    if (canonical == NULL) {
        errno = ENOENT;
        return NULL;
    }
    if (stat(canonical, &status) != 0) {
        free(canonical);
        errno = ENOENT;
        return NULL;
    }
    if (resolved == NULL) return canonical;
    if (strlen(canonical) >= PATH_MAX) {
        free(canonical);
        errno = ENAMETOOLONG;
        return NULL;
    }
    strcpy(resolved, canonical);
    free(canonical);
    return resolved;
}

/*
 * REAL for the four flag commands on a BOUND descriptor; REFUSAL otherwise.
 *
 * fcntl is the single largest caller in this layer (F_GETFD/F_SETFD/F_GETFL/
 * F_SETFL/F_DUPFD/F_DUPFD_CLOEXEC and the record-lock commands), and the whole
 * call used to refuse, because not one of those answers is reachable from the
 * CRT: it exposes no per-descriptor metadata channel at all.
 *
 * What changed is not the CRT.  The four flag commands do not ask the host
 * anything -- close-on-exec, the access mode and the two settable status flags
 * are facts about what the ENGINE asked for when it produced the descriptor,
 * and a host is merely the usual place to keep them.  The handle table keeps
 * them for a bound descriptor, so for those the answers are records, not
 * approximations, and this is the same fcntl the other two hosts implement.
 *
 * An UNBOUND descriptor still refuses, and each of the plausible fakes for it
 * is still worse than ENOSYS, so they stay named:
 *   - F_GETFD returning 0 says "this descriptor is not close-on-exec", which
 *     syscall/proc.c's exec sweep believes and then leaks the fd across execve.
 *   - F_GETFL returning O_RDWR says the fd is readable and writable and neither
 *     append nor nonblocking; io.c reports that word straight back to the guest
 *     as its F_GETFL answer, so a guest that opened O_APPEND would be told it
 *     had not.
 *   - F_SETLK returning 0 grants a lock that nothing is holding, which is the
 *     one failure mode a lock exists to prevent.
 *
 * WHICH IS ALSO THE WARNING.  F_GETFD is used across this layer as an "is this
 * descriptor open?" probe, and the answer this function gives is now "yes" for
 * a bound descriptor and "no" for every other -- which is a statement about the
 * TABLE and not about the descriptor.  That is sound only because a binding is
 * filed under a live descriptor number and is shed the moment that number is
 * closed or reused (fd_reset_emul), so a bound descriptor is always open.  The
 * converse is not true and never will be: an unbound descriptor may be wide
 * open.  A probe therefore under-reports here, exactly as it did when the whole
 * call refused, and no caller may read a refusal as proof of a closed fd.
 *
 * F_DUPFD AND F_DUPFD_CLOEXEC are REAL for an AMBIENT binding and refused
 * otherwise, and the split is the whole of what makes them safe.
 *
 * The body was always the obvious one -- dup in a loop until the result clears
 * the floor, closing the rejects -- and it was refused for a reason that is
 * still true of one caller and only one: syscall/io.c's engine_fd_reloc asks for
 * a floor of 1 << 20 and a dup loop would chase that until it exhausted the
 * 8192-entry table.  So the FLOOR is what is tested.  A floor the table cannot
 * reach is refused exactly as before -- and engine_fd_reloc already retries at
 * 64 when the high floor fails, which is the arm it now takes here -- while a
 * floor inside the table is answered.  The loop is self-limiting: each rejected
 * duplicate holds a slot until the loop ends, so a floor the table can reach is
 * reached in at most one pass over it, and a full table ends the loop with the
 * CRT's own EMFILE instead of spinning.
 *
 * AMBIENT ONLY, because dup(2) duplicates the DESCRIPTOR and only an ambient
 * binding is a descriptor: the int is the object, _dup is exactly dup(2) on it,
 * and the duplicate names the same open file description with its own number.
 * A handle-bound descriptor's object is the handle, which _dup does not
 * duplicate, so the result would be a number pointing at the CRT's idea of the
 * fd and not at the description the caller asked to duplicate.  That refuses.
 *
 * ONE NAMED RESIDUAL.  F_DUPFD_CLOEXEC records close-on-exec, and the record is
 * what this layer reads -- F_GETFD above and syscall/proc.c's emulated execve
 * sweep both answer from it.  It does NOT mark the underlying HANDLE
 * non-inheritable: that is SetHandleInformation, which means <windows.h>, which
 * a header pulled into the guest-target unity TU must not include (see (2)
 * above).  So a descriptor duplicated with this flag is still inheritable by a
 * CreateProcess that inherits handles.  Every caller of these two commands in
 * this layer duplicates an engine-private descriptor that no spawn hands on, so
 * nothing rests on the handle bit today; it is named because the flag's name
 * promises it.
 *
 * The record-lock and SIGIO-owner commands stay refused because nothing records
 * them yet.
 */
static inline int fcntl(int descriptor, int command, ...) {
    hl_host_handle handle;
    uint32_t state = 0;
    int flags = 0;
    if (!hl_fdhandle_lookup_state(descriptor, &handle, &state)) {
        errno = ENOSYS;
        return -1;
    }
    switch (command) {
    case F_GETFD: return (state & HL_FDHANDLE_CLOEXEC) ? FD_CLOEXEC : 0;
    case F_GETFL:
        /* The access mode is the low two bits of the open flag word and is not a
           bit-set: O_RDONLY is 0, so "readable" is the absence of O_WRONLY. */
        flags = (state & HL_FDHANDLE_READ) ? ((state & HL_FDHANDLE_WRITE) ? O_RDWR : O_RDONLY) : O_WRONLY;
        if (state & HL_FDHANDLE_APPEND) flags |= O_APPEND;
        if (state & HL_FDHANDLE_NONBLOCK) flags |= O_NONBLOCK;
        return flags;
    case F_SETFD: {
        /* REAL, and enforced rather than merely recorded: the emulated execve
           sweep in syscall/proc.c reads this flag back through F_GETFD above and
           sheds the binding for every descriptor that carries it. */
        va_list arguments;
        uint32_t updated;
        va_start(arguments, command);
        flags = va_arg(arguments, int);
        va_end(arguments);
        updated = (flags & FD_CLOEXEC) ? (state | HL_FDHANDLE_CLOEXEC) : (state & ~(uint32_t)HL_FDHANDLE_CLOEXEC);
        if (hl_fdhandle_set_state(descriptor, updated) != 0) {
            errno = EBADF;
            return -1;
        }
        return 0;
    }
    case F_SETFL: {
        /* REAL only where it changes nothing, REFUSAL otherwise, and the split
           is deliberate.  O_APPEND and O_NONBLOCK are properties of the open
           file description that the object itself must honour -- the file group
           takes them at open time and has no operation that alters them
           afterwards, and nothing in this layer emulates either one.  Recording
           a change here would therefore answer the guest's next F_GETFL with a
           flag no read or write obeys: a guest that asked for non-blocking would
           be told it had it and would then block forever instead of seeing
           EAGAIN.  A no-op set is still a real success, and runtimes issue one
           routinely after reading the flags back unchanged. */
        va_list arguments;
        uint32_t requested = state & ~HL_FDHANDLE_SETFL_MASK;
        va_start(arguments, command);
        flags = va_arg(arguments, int);
        va_end(arguments);
        if (flags & O_APPEND) requested |= HL_FDHANDLE_APPEND;
        if (flags & O_NONBLOCK) requested |= HL_FDHANDLE_NONBLOCK;
        if (requested == state) return 0;
        errno = ENOSYS;
        return -1;
    }
    case F_DUPFD:
    case F_DUPFD_CLOEXEC: {
        va_list arguments;
        int *held = NULL;
        size_t count = 0;
        size_t capacity = 0;
        int floor;
        int duplicate;
        int error = 0;
        va_start(arguments, command);
        floor = va_arg(arguments, int);
        va_end(arguments);
        /* Not a descriptor this can duplicate: see the AMBIENT-ONLY paragraph. */
        if (handle != HL_HOST_HANDLE_INVALID || (state & HL_FDHANDLE_AMBIENT) == 0) {
            errno = ENOSYS;
            return -1;
        }
        if (floor < 0 || floor >= getdtablesize()) {
            errno = floor < 0 ? EINVAL : ENOSYS;
            return -1;
        }
        for (;;) {
            duplicate = _dup(descriptor);
            if (duplicate < 0) {
                error = errno;
                break;
            }
            if (duplicate >= floor) break;
            if (count == capacity) {
                size_t grown = capacity == 0 ? 64u : capacity * 2u;
                int *resized = (int *)realloc(held, grown * sizeof(*held));
                if (resized == NULL) {
                    _close(duplicate);
                    error = ENOMEM;
                    duplicate = -1;
                    break;
                }
                held = resized;
                capacity = grown;
            }
            held[count++] = duplicate;
        }
        /* The rejects are released only after the winner is in hand, so that the
           loop cannot be handed the same number twice. */
        while (count != 0)
            _close(held[--count]);
        free(held);
        if (duplicate < 0) {
            errno = error;
            return -1;
        }
        /* POSIX: the duplicate does not inherit FD_CLOEXEC, and F_DUPFD_CLOEXEC
           is the spelling that asks for it. */
        (void)hl_fdhandle_publish_ambient(duplicate, command == F_DUPFD_CLOEXEC
                                                         ? (state | HL_FDHANDLE_CLOEXEC)
                                                         : (state & ~(uint32_t)HL_FDHANDLE_CLOEXEC));
        return duplicate;
    }
    default: errno = ENOSYS; return -1;
    }
}

/*
 * REAL for AT_FDCWD, REFUSAL otherwise -- population (1) -- and REFUSAL for a
 * flag the CRT would accept and ignore.  _O_BINARY is added unconditionally:
 * POSIX open performs no newline translation and the CRT's default does.
 *
 * ON SUCCESS IT ALSO FILES AN AMBIENT BINDING, and that is what makes fcntl()
 * above work for a file the guest opened.  The descriptor this returns is the
 * CRT's own, so read/write/lseek keep reaching it directly and nothing here
 * opens a second object for it -- but the CRT then remembers NOTHING about it,
 * and fcntl's four flag commands answer from the record or not at all.  Without
 * the binding they refused, and the refusal was not academic: glibc's fdopen()
 * calls fcntl(fd, F_GETFL) to recover the access mode, so tmpfile() returned
 * NULL for every guest and the caller dereferenced it.  That single missing
 * record was the whole of the libc suite's stdio failures.
 *
 * The state is what the ENGINE asked for, which is the only honest source: the
 * access mode and O_APPEND come from the flag word, O_CLOEXEC from the same,
 * and SEEKABLE is true because this call opens a name in the file system and
 * the CRT gives every such descriptor an offset.  Losing the binding (leaf
 * allocation failure) is not worth failing an open that succeeded -- it costs
 * the fcntl answers, which is exactly where this host was a moment ago.
 */
static inline int openat(int directory, const char *path, int flags, ...) {
    int mode = 0;
    int descriptor;
    uint32_t state;
    if (flags & O_CREAT) {
        va_list arguments;
        va_start(arguments, flags);
        mode = va_arg(arguments, int);
        va_end(arguments);
    }
    if (directory != AT_FDCWD) {
        errno = ENOSYS; /* no directory descriptors on this host */
        return -1;
    }
    if (flags & ~HL_LINUX_HOST_OPEN_ACTIONABLE) {
        errno = ENOSYS; /* O_DIRECTORY / O_NOFOLLOW / O_NONBLOCK / O_PATH / ... */
        return -1;
    }
    descriptor = open(path, (flags & ~O_TEXT) | _O_BINARY, mode);
    if (descriptor < 0) return descriptor;
    /* O_RDONLY is 0, so the access mode is a value in the low two bits and not
       a bit-set -- readable is the absence of O_WRONLY. */
    switch (flags & O_ACCMODE) {
    case O_WRONLY: state = HL_FDHANDLE_WRITE; break;
    case O_RDWR: state = HL_FDHANDLE_READ | HL_FDHANDLE_WRITE; break;
    default: state = HL_FDHANDLE_READ; break;
    }
    state |= HL_FDHANDLE_SEEKABLE;
    if (flags & O_APPEND) state |= HL_FDHANDLE_APPEND;
    if (flags & O_CLOEXEC) state |= HL_FDHANDLE_CLOEXEC;
    (void)hl_fdhandle_publish_ambient(descriptor, state);
    return descriptor;
}

/*
 * REAL for AT_FDCWD, REFUSAL otherwise.  fstatat(AT_FDCWD, p, s, 0) IS stat(p,s)
 * and fstatat(AT_FDCWD, p, s, AT_SYMLINK_NOFOLLOW) IS lstat(p, s) -- with
 * lstat's residual, above, and no other.  AT_EMPTY_PATH names the descriptor
 * itself, which is population (1) again.
 */
static inline int fstatat(int directory, const char *path, struct stat *status, int flags) {
    if ((flags & AT_EMPTY_PATH) || directory != AT_FDCWD) {
        errno = ENOSYS;
        return -1;
    }
    return (flags & AT_SYMLINK_NOFOLLOW) ? lstat(path, status) : stat(path, status);
}

/* REAL for AT_FDCWD, REFUSAL otherwise.  AT_REMOVEDIR is what selects rmdir(2)
 * over unlink(2), which is the whole content of unlinkat's flag argument. */
static inline int unlinkat(int directory, const char *path, int flags) {
    if (directory != AT_FDCWD) {
        errno = ENOSYS;
        return -1;
    }
    return (flags & AT_REMOVEDIR) ? rmdir(path) : unlink(path);
}

/* REAL for AT_FDCWD, REFUSAL otherwise, with the mode dropped -- named, because
 * dropping an argument is exactly the kind of quiet loss this file is supposed
 * to label.  The UCRT's _mkdir takes no mode and Windows has no POSIX
 * permission bits to apply one to; the directory is created with the inherited
 * ACL.  Refusing instead would refuse mkdir(2) entirely on this host, and the
 * permission bits are the part of the request Windows cannot represent rather
 * than a part it silently gets wrong. */
static inline int mkdirat(int directory, const char *path, mode_t mode) {
    (void)mode;
    if (directory != AT_FDCWD) {
        errno = ENOSYS;
        return -1;
    }
    return _mkdir(path);
}

/*
 * REAL for AT_FDCWD, REFUSAL otherwise, and the X_OK residual is named.
 * The UCRT's _access has no execute question -- it answers 0 for any readable
 * file (measured) -- because Windows carries executability in the ACL and the
 * image header rather than in a mode bit.  So X_OK here means F_OK, which is
 * permissive: a guest asking "can I exec this" gets yes where Linux would say
 * EACCES.  Refusing the whole call would break the R_OK/W_OK/F_OK questions
 * that ARE answered correctly, so the residual is kept and labelled.
 * AT_EACCESS (ask about the effective ids) is vacuous on a host with no ids.
 */
static inline int faccessat(int directory, const char *path, int mode, int flags) {
    (void)flags;
    if (directory != AT_FDCWD) {
        errno = ENOSYS;
        return -1;
    }
    return access(path, mode & ~X_OK);
}

/*
 * REFUSAL -- population (3).  The CRT has no link, symlink or readlink at all,
 * on any path, with or without a dirfd, so these do not get an AT_FDCWD arm:
 * there is nothing to forward to.  Win32 does have CreateSymbolicLinkW,
 * CreateHardLinkW and the reparse-point ioctl, so these become REAL in the host
 * backend rather than never.
 *
 * readlink() answering EINVAL ("not a symbolic link") was considered and
 * rejected: that is a plausible success in disguise, and container/vfs/resolve.c
 * reads it as "this component is a real file, stop walking".
 */
static inline int link(const char *existing, const char *created) {
    (void)existing;
    (void)created;
    errno = ENOSYS;
    return -1;
}

static inline int linkat(int existing_directory, const char *existing, int created_directory, const char *created,
                         int flags) {
    (void)existing_directory;
    (void)existing;
    (void)created_directory;
    (void)created;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

static inline int symlink(const char *target, const char *created) {
    (void)target;
    (void)created;
    errno = ENOSYS;
    return -1;
}

static inline int symlinkat(const char *target, int created_directory, const char *created) {
    (void)target;
    (void)created_directory;
    (void)created;
    errno = ENOSYS;
    return -1;
}

static inline ssize_t readlink(const char *path, char *buffer, size_t capacity) {
    (void)path;
    (void)buffer;
    (void)capacity;
    errno = ENOSYS;
    return -1;
}

static inline ssize_t readlinkat(int directory, const char *path, char *buffer, size_t capacity) {
    (void)directory;
    (void)path;
    (void)buffer;
    (void)capacity;
    errno = ENOSYS;
    return -1;
}

/*
 * REFUSAL -- population (3).  Windows has no POSIX mode bits: _chmod toggles a
 * single read-only attribute and nothing else, so honouring an fchmod would
 * mean claiming to have set nine permission bits after setting one.  The mode a
 * guest reads back comes from this layer's own metadata, not from the host.
 */
static inline int fchmod(int descriptor, mode_t mode) {
    (void)descriptor;
    (void)mode;
    errno = ENOSYS;
    return -1;
}

static inline int fchmodat(int directory, const char *path, mode_t mode, int flags) {
    (void)directory;
    (void)path;
    (void)mode;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL -- population (3).  Device and FIFO nodes have no filesystem
 * representation here at all; a Windows named pipe is a namespace object, not a
 * directory entry. */
static inline int mknodat(int directory, const char *path, mode_t mode, dev_t device) {
    (void)directory;
    (void)path;
    (void)mode;
    (void)device;
    errno = ENOSYS;
    return -1;
}

/*
 * REFUSAL -- population (3).  The CRT's _utime/_futime take whole seconds, and
 * neither has any way to express UTIME_OMIT ("leave this one alone").  A guest
 * setting only mtime would have its atime rewritten to the truncated value, and
 * a guest reading back a nanosecond timestamp it just wrote would find it
 * changed -- both are silent data edits, so the second-granularity call is not
 * offered.  Windows keeps 100 ns timestamps in FILE_BASIC_INFORMATION, so this
 * is a CRT limit and not a platform one.
 */
static inline int utimensat(int directory, const char *path, const struct timespec *times, int flags) {
    (void)directory;
    (void)path;
    (void)times;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

/*
 * REAL for a bound descriptor, REFUSAL otherwise.  The paragraph above is about
 * the CRT and stays true of it; the file group's set_times is a different
 * primitive with the two properties the CRT lacks -- a per-timestamp
 * NOW/OMIT/EXPLICIT selector, and nanoseconds -- so for a descriptor that names
 * a handle this is the operation utimensat is defined as, not a rounding of it.
 * A NULL `times` is "set both to now", which is that selector's other value.
 */
static inline int futimens(int descriptor, const struct timespec *times) {
    const hl_host_services *host = hl_fdhandle_host();
    hl_host_handle handle;
    hl_host_file_time applied[2];
    hl_host_result result;
    int index;
    if (!hl_fdhandle_lookup(descriptor, &handle) || host == NULL || host->file == NULL ||
        host->file->set_times == NULL) {
        errno = ENOSYS;
        return -1;
    }
    for (index = 0; index < 2; index++) {
        long nanoseconds = times == NULL ? UTIME_NOW : times[index].tv_nsec;
        applied[index].mode = HL_HOST_FILE_TIME_EXPLICIT;
        applied[index].seconds = times == NULL ? 0 : (int64_t)times[index].tv_sec;
        applied[index].nanoseconds = times == NULL ? 0 : (uint32_t)times[index].tv_nsec;
        if (nanoseconds == UTIME_NOW) {
            applied[index].mode = HL_HOST_FILE_TIME_NOW;
            applied[index].seconds = 0;
            applied[index].nanoseconds = 0;
        } else if (nanoseconds == UTIME_OMIT) {
            applied[index].mode = HL_HOST_FILE_TIME_OMIT;
            applied[index].seconds = 0;
            applied[index].nanoseconds = 0;
        } else if (nanoseconds < 0 || nanoseconds >= 1000000000L) {
            errno = EINVAL;
            return -1;
        }
    }
    result = host->file->set_times(host->context, handle, applied);
    if (result.status != HL_STATUS_OK) {
        errno = hl_fdhandle_errno(result.status);
        return -1;
    }
    return 0;
}

/*
 * REAL for a bound descriptor, REFUSAL otherwise.  There is no positional read
 * or write in the CRT, and the seek/read/seek synthesis is refused for the
 * reason host_uio.h refuses a writev loop, only sharper: pread(2) is defined to
 * leave the file offset untouched and to be atomic against a concurrent
 * reader, and a thread that seeks between this thread's seek and its read gets
 * the WRONG BYTES rather than merely interleaved ones.  Windows answers this
 * exactly, with an OVERLAPPED offset on ReadFile/WriteFile -- and the file
 * group's read_at/write_at are that answer, reachable once the descriptor names
 * a handle.
 */
static inline ssize_t pread(int descriptor, void *buffer, size_t count, off_t offset) {
    const hl_host_services *host = hl_fdhandle_host();
    hl_host_handle handle;
    hl_host_bytes output;
    hl_host_result result;
    if (offset < 0) {
        errno = EINVAL;
        return -1;
    }
    if (!hl_fdhandle_lookup(descriptor, &handle) || host == NULL || host->file == NULL || host->file->read_at == NULL) {
        errno = ENOSYS;
        return -1;
    }
    output.data = buffer;
    output.size = count;
    result = host->file->read_at(host->context, handle, (uint64_t)offset, output);
    if (result.status != HL_STATUS_OK) {
        errno = hl_fdhandle_errno(result.status);
        return -1;
    }
    return (ssize_t)result.value;
}

static inline ssize_t pwrite(int descriptor, const void *buffer, size_t count, off_t offset) {
    const hl_host_services *host = hl_fdhandle_host();
    hl_host_handle handle;
    hl_host_const_bytes input;
    hl_host_result result;
    if (offset < 0) {
        errno = EINVAL;
        return -1;
    }
    if (!hl_fdhandle_lookup(descriptor, &handle) || host == NULL || host->file == NULL ||
        host->file->write_at == NULL) {
        errno = ENOSYS;
        return -1;
    }
    input.data = buffer;
    input.size = count;
    result = host->file->write_at(host->context, handle, (uint64_t)offset, input);
    if (result.status != HL_STATUS_OK) {
        errno = hl_fdhandle_errno(result.status);
        return -1;
    }
    return (ssize_t)result.value;
}

/*
 * REFUSAL -- population (1).  fchdir needs the path behind a descriptor, and
 * the descriptor cannot exist: the CRT will not open a directory.  Even given
 * one, recovering its path means GetFinalPathNameByHandleW, which is the
 * backend's to call.
 */
static inline int fchdir(int descriptor) {
    (void)descriptor;
    errno = ENOSYS;
    return -1;
}

/*
 * REFUSAL, and the only one in this file whose prototype has nowhere to put the
 * refusal: sync(2) returns void.  So it sets errno and does nothing, and the
 * guest's sync(2) will still return 0 -- stated plainly because it is the one
 * place here a caller cannot tell.  _flushall() was considered and rejected: it
 * flushes the CRT's own stream buffers, which is not what sync(2) promises and
 * would make the no-op look like an implementation.  Windows has no
 * whole-system flush short of enumerating volumes and FlushFileBuffers-ing each.
 */
static inline void sync(void) {
    errno = ENOSYS;
}

/* kill(2) is NOT here.  It reads as descriptor-adjacent because it travels with
 * the pid vocabulary below, but it is <signal.h>'s and host_signal.h defines it;
 * two force-included headers defining the same static inline is a redefinition
 * error, which is how this was found. */

/*
 * REFUSAL -- population (3).  Windows has no uid/gid at all; identity is a SID
 * in the process token, which does not fit in a uid_t and has no ordering that
 * "0 means root" would respect.
 *
 * Returning 0 is the fake that matters here and it is a privilege claim:
 * container/state.c's cuid()/cgid() fall back to getuid()/getgid() for the
 * engine's own identity, and syscall paths gate on uid 0.  (uid_t)-1 is the
 * POSIX "no such user" value and no id equals it, so those fallbacks compare
 * unequal instead of comparing equal to root.  errno is set even though POSIX
 * says these calls cannot fail, because on this host they did.
 */
static inline uid_t getuid(void) {
    errno = ENOSYS;
    return (uid_t)-1;
}

static inline gid_t getgid(void) {
    errno = ENOSYS;
    return (gid_t)-1;
}

static inline int getgroups(int capacity, gid_t *groups) {
    (void)capacity;
    (void)groups;
    errno = ENOSYS;
    return -1;
}

static inline int setregid(gid_t real_group, gid_t effective_group) {
    (void)real_group;
    (void)effective_group;
    errno = ENOSYS;
    return -1;
}

/*
 * REFUSAL -- population (3).  Windows has no process groups and no sessions.
 * Job objects are the nearest thing and they are neither: a job has no leader,
 * no controlling terminal and no membership a child inherits by pid.
 *
 * -1 rather than a plausible pid, for the same reason as the ids above:
 * container/vfs.c writes getpgid(0)/getsid(0) into the synthesized
 * /proc/<pid>/stat fields 5 and 6, checkpoint.c compares getsid(0) against
 * getpid() to decide whether this process WAS a session leader, and getppid()
 * against g_init_hostpid to decide whether it is the container init's child.
 * Every one of those tests is a comparison, so an invented pid would not fail
 * -- it would answer, wrongly and quietly.
 *
 * getppid() is the exception, and it is REAL: it was already named here as
 * recoverable from ntdll's PROCESS_BASIC_INFORMATION and belonging to the
 * backend, and now that guest fork(2) creates real parents it has a real
 * question to answer.  One difference from Linux is worth knowing at the call
 * sites: Windows does not re-parent an orphan, so a child whose parent has
 * exited reports the dead parent's id where Linux would report 1.
 */
static inline pid_t getppid(void) {
    const int parent = hl_host_windows_parent_pid();
    if (parent < 0) {
        errno = ENOSYS;
        return (pid_t)-1;
    }
    return (pid_t)parent;
}

static inline pid_t getpgrp(void) {
    errno = ENOSYS;
    return (pid_t)-1;
}

static inline pid_t getpgid(pid_t process) {
    (void)process;
    errno = ENOSYS;
    return (pid_t)-1;
}

static inline pid_t getsid(pid_t process) {
    (void)process;
    errno = ENOSYS;
    return (pid_t)-1;
}

static inline int setpgid(pid_t process, pid_t group) {
    (void)process;
    (void)group;
    errno = ENOSYS;
    return -1;
}

static inline pid_t setsid(void) {
    errno = ENOSYS;
    return (pid_t)-1;
}

#endif /* _WIN32 */

#endif
