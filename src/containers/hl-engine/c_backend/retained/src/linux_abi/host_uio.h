#ifndef HL_LINUX_ABI_HOST_UIO_H
#define HL_LINUX_ABI_HOST_UIO_H

/*
 * <sys/uio.h> for this layer.  Same construction and the same REAL/SHAPE/REFUSAL
 * labelling as host_mman.h; see that file's header for why this vocabulary lives
 * in src/linux_abi rather than in src/host/native_compat.h.
 *
 * The split here is unusually clean, and worth stating because it is the reason
 * this block is small rather than large.  `struct iovec` appears in this layer in
 * two roles that happen to share a spelling:
 *
 *   - As the GUEST's iovec.  syscall/guest_copy.c's guest_iov_import and every
 *     iovec path in sentry.c decode a Linux `struct iovec[]` out of guest memory.
 *     The layout that matters there is the guest's -- {void *, size_t}, 16 bytes
 *     on both supported guest ISAs -- and the host type is used only because on
 *     Linux and macOS it happens to be identical.  Defining that layout on a host
 *     that does not supply one is not an approximation of anything; it is the
 *     guest ABI, written down.  SHAPE.
 *
 *   - As an argument to a host readv/writev on an int fd.  That is the other
 *     population entirely, and it splits by SEGMENT COUNT rather than by host.
 *
 * The split is where an earlier revision of this file was wrong, and the
 * correction is worth recording because it was found by running a guest rather
 * than by reading code.  That revision refused both calls outright on the
 * grounds that "a guest fd number names no host object on Windows".  That is
 * true of a descriptor the engine would have to open through the file seam, and
 * false of the ones a guest actually starts with: the standard streams and
 * everything the CRT opens ARE ordinary CRT descriptors here, and write(2) on
 * them works.  The measured consequence of the blanket refusal was a guest whose
 * write(1, "hi\n", 3) returned -ENOSYS -- because this layer routes every plain
 * write through guest_fd_write, which uses writev.
 *
 * So:
 *
 *   - count <= 1 is REAL.  A one-segment vector is a plain read/write by
 *     definition; there is no gather and therefore nothing to be atomic about.
 *     This is not an approximation of writev, it IS writev for that input.
 *
 *   - count > 1 was a REFUSAL, on reasoning that is unchanged and still correct:
 *     POSIX writev is atomic with respect to the file offset on a regular file
 *     and atomic up to PIPE_BUF on a pipe.  A loop of writes is neither, so a
 *     guest relying on either would see interleaving -- a wrong answer --
 *     instead of an error.  The refusal named its own fix: "the seam already
 *     carries readv/writev/readv_at/writev_at in the file group keyed on an
 *     hl_host_handle, so the gather becomes REAL the moment the
 *     descriptor-to-HANDLE binding exists; there is no missing seam operation
 *     here, only a missing binding."
 *
 * THE BINDING NOW EXISTS (fdhandle.h), and this file is the first consumer of
 * it.  So the split is no longer by segment count alone, it is by binding:
 *
 *   - A BOUND descriptor is REAL at every count, and at every count it goes
 *     through the seam rather than the CRT -- not only the gather.  Splitting by
 *     count for a bound descriptor would be the actual bug: the two paths reach
 *     the object through different references, and for anything with a file
 *     offset that is two positions where the guest has one.
 *
 *   - An UNBOUND descriptor keeps exactly the behaviour above: one segment is
 *     REAL through the CRT, more than one is the same refusal for the same
 *     reason.  A miss is not an error and must not become one -- see fdhandle.h
 *     on why the table can only ever add answers.
 *
 * preadv/pwritev were a flat refusal because the CRT has no positional I/O at
 * all.  For a bound descriptor they are readv_at/writev_at, which is the same
 * operation and not an approximation of it; unbound they still refuse.
 */

#if !defined(_WIN32)

#include <sys/uio.h>

#else /* Windows */

#include "fdhandle.h"

#include <errno.h>
#include <io.h>
#include <stddef.h>
#include <stdlib.h>
#include <sys/types.h>

/* SHAPE.  The guest ABI's iovec, which is also POSIX's. */
struct iovec {
    void *iov_base;
    size_t iov_len;
};

#ifndef IOV_MAX
#define IOV_MAX 1024
#endif
#define UIO_MAXIOV IOV_MAX

/* SHAPE: preadv2/pwritev2 flag words, Linux values. */
#define RWF_HIPRI 0x00000001
#define RWF_DSYNC 0x00000002
#define RWF_SYNC 0x00000004
#define RWF_NOWAIT 0x00000008
#define RWF_APPEND 0x00000010

/* A zero-length transfer still has to reach the descriptor: write(fd, ., 0) is
 * how a caller probes that fd is open for writing, and returning 0 without
 * asking would answer that probe wrongly for a closed or read-only fd. */
#define HL_LINUX_UIO_BASE(v, n) ((n) > 0 ? (v)[0].iov_base : NULL)
#define HL_LINUX_UIO_LEN(v, n) ((n) > 0 ? (v)[0].iov_len : (size_t)0)

/* The seam's vector type is not this one -- it carries the base as a uint64_t so
 * that a 32-bit host and a 64-bit guest can share the shape -- so the array is
 * converted rather than cast.  A cast between two distinct struct types would be
 * an aliasing violation even where the layouts agree, and the conversion is a
 * pair of loads per segment against a syscall.  Vectors up to this many segments
 * convert on the stack; POSIX allows up to IOV_MAX and those allocate. */
enum { HL_LINUX_UIO_INLINE = 16 };

/* Returns `stack` for a short vector, a fresh allocation for a long one, or NULL
 * when that allocation fails.  The caller frees only what is not `stack`. */
static inline hl_host_iovec *hl_linux_uio_convert(const struct iovec *vectors, int count, hl_host_iovec *stack) {
    hl_host_iovec *converted = stack;
    int index;
    if (count > HL_LINUX_UIO_INLINE) {
        converted = (hl_host_iovec *)malloc((size_t)count * sizeof *converted);
        if (converted == NULL) return NULL;
    }
    for (index = 0; index < count; index++) {
        converted[index].address = (uint64_t)(uintptr_t)vectors[index].iov_base;
        converted[index].size = (uint64_t)vectors[index].iov_len;
    }
    return converted;
}

/* The four vectored operations differ only in which file-group entry point they
 * reach and whether an offset participates, so they share one body.  `positional`
 * selects readv_at/writev_at over readv/writev; `offset` is ignored without it. */
static inline ssize_t hl_linux_uio_seam(hl_host_handle handle, const struct iovec *vectors, int count, int writing,
                                        int positional, off_t offset) {
    const hl_host_services *host = hl_fdhandle_host();
    const hl_host_file_services *file = host == NULL ? NULL : host->file;
    hl_host_iovec stack[HL_LINUX_UIO_INLINE];
    hl_host_iovec *converted;
    hl_host_result result;
    if (file == NULL) {
        errno = ENOSYS;
        return -1;
    }
    if (positional ? (writing ? file->writev_at == NULL : file->readv_at == NULL)
                   : (writing ? file->writev == NULL : file->readv == NULL)) {
        errno = ENOSYS;
        return -1;
    }
    converted = hl_linux_uio_convert(vectors, count, stack);
    if (converted == NULL) {
        errno = ENOMEM;
        return -1;
    }
    if (positional)
        result = writing ? file->writev_at(host->context, handle, converted, (uint32_t)count, (uint64_t)offset)
                         : file->readv_at(host->context, handle, converted, (uint32_t)count, (uint64_t)offset);
    else
        result = writing ? file->writev(host->context, handle, converted, (uint32_t)count)
                         : file->readv(host->context, handle, converted, (uint32_t)count);
    if (converted != stack) free(converted);
    if (result.status != HL_STATUS_OK) {
        errno = hl_fdhandle_errno(result.status);
        return -1;
    }
    return (ssize_t)result.value;
}

/*
 * A multi-segment transfer over a descriptor that has no handle: walk the
 * segments with the CRT's own read/write, stopping on the first short or failed
 * one exactly as the kernel does.
 *
 * WHY THIS IS SOUND HERE AND NOT IN GENERAL.  readv/writev differ from a loop in
 * one respect only: the kernel performs them as a single operation against the
 * file offset, so a concurrent transfer on the same description cannot interleave
 * between segments.  That distinction is observable on a pipe, a socket or a
 * terminal, where a second writer exists and PIPE_BUF atomicity is specified --
 * so this path is taken ONLY for a descriptor the engine recorded as SEEKABLE,
 * which on this host means a regular file that openat() opened by name.  For a
 * regular file the loop and the vectored call transfer the same bytes to the
 * same offsets in the same order.
 *
 * THE RESIDUAL DIVERGENCE, stated rather than argued away: Linux performs a
 * writev to a regular file under the inode lock, so two guest threads writing
 * the same description cannot interleave their segments.  This loop can.  A
 * guest that concurrently writev()s a shared regular-file descriptor from more
 * than one thread may therefore observe segment interleaving it would not see
 * on Linux.  Nothing in the corpus exercises that shape, which is why this is a
 * comment and not a refusal -- but it is a real difference, and the fix is the
 * descriptor-to-handle routing that makes file->writev reachable here, not a
 * wider loop.  Until then the trade is deliberate: without this path a guest
 * doing readv/writev on a file it opened got ENOSYS for the whole call, which
 * fails every time rather than under concurrency.
 *
 * Anything else still refuses.  It is the count > 1 refusal that was reached in
 * practice: a guest doing readv/writev over a file it opened got ENOSYS for the
 * whole call, which is why the vectored-IO cases failed with wrote=-1 read=-1.
 */
static inline ssize_t hl_linux_uio_ambient(int fd, const struct iovec *vectors, int count, int writing) {
    ssize_t total = 0;
    int index;
    for (index = 0; index < count; index++) {
        void *base = vectors[index].iov_base;
        size_t length = vectors[index].iov_len;
        int moved;
        if (length == 0) continue;
        moved = writing ? _write(fd, base, (unsigned int)length) : _read(fd, base, (unsigned int)length);
        if (moved < 0) return total > 0 ? total : (ssize_t)-1; /* errno already set */
        total += moved;
        if ((size_t)moved < length) break; /* short transfer ends the call */
    }
    return total;
}

/* Returns 1 when `fd` carries an ambient binding the loop above may serve. */
static inline int hl_linux_uio_ambient_ok(int fd) {
    hl_host_handle handle;
    uint32_t state = 0;
    if (!hl_fdhandle_lookup_state(fd, &handle, &state)) return 0;
    return (state & HL_FDHANDLE_AMBIENT) && (state & HL_FDHANDLE_SEEKABLE);
}

/* REAL for a bound descriptor at any count; REAL for one segment on an unbound
 * one, and for any count on a SEEKABLE ambient one; REFUSAL above that -- see
 * the header note.  ENOSYS and not EBADF for the refusal: EBADF would tell the
 * guest its descriptor is wrong, which it is not.  EINVAL is reserved for a
 * genuinely malformed count. */
static inline ssize_t readv(int fd, const struct iovec *vectors, int count) {
    hl_host_handle handle;
    if (count < 0 || count > IOV_MAX || (count > 0 && vectors == NULL)) {
        errno = EINVAL;
        return -1;
    }
    if (hl_fdhandle_lookup(fd, &handle)) return hl_linux_uio_seam(handle, vectors, count, 0, 0, 0);
    if (count > 1) {
        if (hl_linux_uio_ambient_ok(fd)) return hl_linux_uio_ambient(fd, vectors, count, 0);
        errno = ENOSYS;
        return -1;
    }
    return (ssize_t)_read(fd, HL_LINUX_UIO_BASE(vectors, count), (unsigned int)HL_LINUX_UIO_LEN(vectors, count));
}

static inline ssize_t writev(int fd, const struct iovec *vectors, int count) {
    hl_host_handle handle;
    if (count < 0 || count > IOV_MAX || (count > 0 && vectors == NULL)) {
        errno = EINVAL;
        return -1;
    }
    if (hl_fdhandle_lookup(fd, &handle)) return hl_linux_uio_seam(handle, vectors, count, 1, 0, 0);
    if (count > 1) {
        if (hl_linux_uio_ambient_ok(fd)) return hl_linux_uio_ambient(fd, vectors, count, 1);
        errno = ENOSYS;
        return -1;
    }
    return (ssize_t)_write(fd, HL_LINUX_UIO_BASE(vectors, count), (unsigned int)HL_LINUX_UIO_LEN(vectors, count));
}

/* REAL for a bound descriptor, REFUSAL otherwise: the CRT has no positional I/O
 * at all, so there is nothing to fall back to. */
static inline ssize_t preadv(int fd, const struct iovec *vectors, int count, off_t offset) {
    hl_host_handle handle;
    if (count < 0 || count > IOV_MAX || (count > 0 && vectors == NULL) || offset < 0) {
        errno = EINVAL;
        return -1;
    }
    if (!hl_fdhandle_lookup(fd, &handle)) {
        errno = ENOSYS;
        return -1;
    }
    return hl_linux_uio_seam(handle, vectors, count, 0, 1, offset);
}

static inline ssize_t pwritev(int fd, const struct iovec *vectors, int count, off_t offset) {
    hl_host_handle handle;
    if (count < 0 || count > IOV_MAX || (count > 0 && vectors == NULL) || offset < 0) {
        errno = EINVAL;
        return -1;
    }
    if (!hl_fdhandle_lookup(fd, &handle)) {
        errno = ENOSYS;
        return -1;
    }
    return hl_linux_uio_seam(handle, vectors, count, 1, 1, offset);
}

static inline ssize_t preadv2(int fd, const struct iovec *vectors, int count, off_t offset, int flags) {
    (void)flags;
    return preadv(fd, vectors, count, offset);
}

static inline ssize_t pwritev2(int fd, const struct iovec *vectors, int count, off_t offset, int flags) {
    (void)flags;
    return pwritev(fd, vectors, count, offset);
}

#endif /* _WIN32 */

#endif
