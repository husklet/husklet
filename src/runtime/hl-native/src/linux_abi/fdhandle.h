#ifndef HL_LINUX_ABI_FDHANDLE_H
#define HL_LINUX_ABI_FDHANDLE_H

/*
 * The guest-descriptor -> hl_host_handle table.
 *
 * WHAT IT IS FOR.  Every typed host service that operates on an open object --
 * the file group's readv/writev/readv_at/writev_at/clone_for_fork/metadata/
 * set_times/set_permissions, and the memory group's map_file/unmap_range -- is
 * keyed on an hl_host_handle.  This layer, everywhere outside the typed
 * hl_linux_abi box, is keyed on an int.  On Linux and macOS that int IS the
 * kernel object, so the two namespaces coincide by accident of history and the
 * seam is never needed; on a host where it is not, every one of those
 * operations is unreachable not because the operation is missing but because
 * no caller holds a handle.  This table is where a caller holds one.
 *
 * It is deliberately NOT a second descriptor allocator.  It allocates no fd
 * numbers, has no lowest-free rule, and answers no question about whether a
 * descriptor is open.  It is a SIDE TABLE keyed on a descriptor number that
 * some other allocator already produced -- structurally the same kind of thing
 * as the pipe-identity, eventfd-peer and fd-path tables the syscall layer
 * already carries, and cleared by the same function they are (fd_reset_emul).
 * That is the whole invariant: a binding here is only ever as valid as the
 * descriptor number it is filed under, so the moment that number stops meaning
 * what it meant, the binding goes with it.  Nothing else in this file matters
 * as much as that sentence.
 *
 * WHY A MISS IS NOT AN ERROR.  A descriptor with no binding is the normal case
 * and always will be on Linux and macOS, where nothing publishes into this
 * table at all.  Callers therefore treat a miss as "take the ambient path",
 * never as "this descriptor is bad".  A consumer that cannot take an ambient
 * path -- a host with no ambient path to take -- returns its own refusal, which
 * is what it returned before this table existed.  So adding a binding can only
 * turn a refusal into an answer and can never turn an answer into a refusal.
 *
 * OWNERSHIP.  publish() transfers the handle to the table.  release() closes it
 * through the file group; forget() drops the binding without closing, for the
 * one caller that has handed the handle somewhere else.  clone() is dup(2)'s
 * half of the contract: it asks the file group for an independent handle onto
 * the same open file description, which is exactly what clone_for_fork is
 * specified to produce.
 *
 * CONCURRENCY.  Lookup is the hot path -- it sits under every vectored read and
 * write -- and is a single relaxed load of a leaf pointer plus an acquire load
 * of a slot, with no lock.  Mutation publishes the state word before the handle
 * and retires the handle before the state, so a lookup that observes a live
 * handle has already observed the state that goes with it.  Leaves are claimed
 * with a compare-exchange and are never freed, so a pointer a reader loaded
 * stays valid for the life of the process.
 *
 * The state word carries exactly the per-descriptor and per-description facts a
 * host may be unable to answer for itself: close-on-exec, the access mode, and
 * the two status flags a guest can set after the fact.  It is not a cache of
 * anything the host knows -- it is the record of what the engine asked for.
 */

#include "hl/host_services.h"

#include <stdint.h>

enum {
    /* FD_CLOEXEC.  A property of the descriptor, not of the description. */
    HL_FDHANDLE_CLOEXEC = 1u << 0,
    /* The access mode the handle was opened with.  Both set is O_RDWR. */
    HL_FDHANDLE_READ = 1u << 1,
    HL_FDHANDLE_WRITE = 1u << 2,
    /* Description status flags a guest may set with F_SETFL after the open. */
    HL_FDHANDLE_APPEND = 1u << 3,
    HL_FDHANDLE_NONBLOCK = 1u << 4,
    /* The object has a file offset -- false for a stream, pipe or terminal. */
    HL_FDHANDLE_SEEKABLE = 1u << 5,
    /* AMBIENT: this binding carries STATE ONLY and holds no host handle.
     *
     * It exists for the descriptor a host produced through its own ambient
     * descriptor namespace -- the UCRT's open() on Windows -- where the int
     * already names the object, so no handle is needed to read or write it, but
     * the per-descriptor FACTS the guest may ask for later still have nowhere
     * to live: the CRT exposes no per-descriptor metadata channel, so
     * close-on-exec, the access mode and the two settable status flags are
     * unanswerable unless the engine records what it asked for.
     *
     * The distinction is load-bearing rather than cosmetic. hl_fdhandle_lookup()
     * -- the handle accessor every typed consumer uses (readv/writev, map_file,
     * the logical VMA ledger) -- reports an ambient binding as a MISS, because
     * there genuinely is no handle and those consumers must keep taking the
     * ambient path they take today. Only hl_fdhandle_lookup_state(), whose
     * callers read the state word and not the handle, sees it. That is what
     * keeps this from becoming the "two positions for one file" hazard: an
     * ambient descriptor has exactly ONE position, the host's, because nothing
     * here ever opens a second object for it. */
    HL_FDHANDLE_AMBIENT = 1u << 6
};

/* The status-flag subset F_SETFL is allowed to change. */
#define HL_FDHANDLE_SETFL_MASK ((uint32_t)(HL_FDHANDLE_APPEND | HL_FDHANDLE_NONBLOCK))

/* Bind the host services the table closes and clones handles through. Idempotent;
   a rebind with the same services is a no-op. Safe to call before any publish. */
void hl_fdhandle_bind(const hl_host_services *host);
const hl_host_services *hl_fdhandle_host(void);

/* 0 on success, -1 if the descriptor is out of range or memory is exhausted.
   Publishing over a live binding releases the one it replaced. */
int hl_fdhandle_publish(int descriptor, hl_host_handle handle, uint32_t state);

/* Record STATE ONLY for a descriptor whose object is the host's own ambient
   descriptor -- see HL_FDHANDLE_AMBIENT. Owns nothing and closes nothing; the
   descriptor is closed by whoever opened it. 0 on success, -1 out of range. */
int hl_fdhandle_publish_ambient(int descriptor, uint32_t state);

/* 1 and fills *handle when a HANDLE is bound; 0 otherwise -- and an ambient
   binding is deliberately a 0 here, because it holds no handle. *handle is
   untouched on a miss. */
int hl_fdhandle_lookup(int descriptor, hl_host_handle *handle);
/* 1 when a binding of EITHER kind exists. *handle is HL_HOST_HANDLE_INVALID for
   an ambient one, so a caller that reads it must check. */
int hl_fdhandle_lookup_state(int descriptor, hl_host_handle *handle, uint32_t *state);

/* Replace the state word of a live binding. 0 on success, -1 if unbound. */
int hl_fdhandle_set_state(int descriptor, uint32_t state);

/* Drop the binding without closing the handle. Returns 1 if one was dropped. */
int hl_fdhandle_forget(int descriptor);

/* Drop the binding and close the handle. Returns 1 if one was closed. */
int hl_fdhandle_release(int descriptor);

/* dup(2): an independent handle onto the same description, filed under `to`.
   0 on success, -1 if `from` is unbound or the host cannot clone. */
int hl_fdhandle_clone(int from, int to);

/* execve: release every binding marked close-on-exec. Returns the count. */
int hl_fdhandle_release_cloexec(void);

/* Bindings currently live. The one thing a caller may ask about the table as a
   whole; used by tests and by the ambient fast paths that skip work when empty. */
uint32_t hl_fdhandle_population(void);

/*
 * Adopt this process's three standard streams into the table.  Idempotent, and
 * a no-op wherever the file group has no standard_stream operation.
 *
 * This is the ONE producer the table starts with, and it is the right one to
 * start with for a reason worth stating: descriptors 0, 1 and 2 are the only
 * ones a guest is handed rather than asking for, so they are the only ones that
 * exist before any open path has run.  Every other producer -- open, pipe,
 * socket, accept -- publishes at the point it creates the descriptor.
 *
 * Returns the number of streams adopted (0..3).
 */
int hl_fdhandle_adopt_standard_streams(void);

/*
 * hl_status -> the errno a POSIX-shaped caller over this table reports.
 *
 * It lives here because every consumer of the table needs it and no two of them
 * can include each other: the seam headers that stand in for <sys/uio.h>,
 * <fcntl.h> and <sys/mman.h> are pulled into the target unit in an order none
 * of them controls, and the ledger sources are not in that unit at all.  This
 * header is the one thing all of them already include.
 *
 * The errno numbers are the HOST's, matching every other refusal in those
 * headers -- the guest-facing translation happens once, later, at the syscall
 * boundary.  Statuses with no POSIX counterpart land on EIO rather than on a
 * number that would claim something specific and wrong.
 *
 * COMPILED ONLY WHERE IT IS CALLED, and the guard is not cosmetic: reaching the
 * numbers means <errno.h>, and this directory contains a FILE OF THAT NAME.
 * A translation unit built with -I on this directory -- which is how the unit
 * tests compile against it -- resolves the angle-bracket include to that file
 * and gets one function prototype instead of the E* constants.  Every caller of
 * this mapper is in a host arm that has already included the real header
 * through its own <errno.h> for its own refusals, so the guard costs nothing
 * and the trap cannot be stepped on from a test.
 */
#if defined(_WIN32)
#include <errno.h>
#if !defined(EINVAL)
/* One legible line instead of twenty "undeclared identifier" lines: the include
   above found this directory's own errno.h, which declares one prototype and no
   E* constants. Compile this unit without -I on src/linux_abi. */
#error "<errno.h> resolved to src/linux_abi/errno.h -- that directory is on the angle-bracket include path"
#endif

static inline int hl_fdhandle_errno(int32_t status) {
    switch (status) {
    case HL_STATUS_OK: return 0;
    case HL_STATUS_INVALID_ARGUMENT: return EINVAL;
    case HL_STATUS_NOT_SUPPORTED: return ENOSYS;
    case HL_STATUS_OUT_OF_MEMORY: return ENOMEM;
    case HL_STATUS_RESOURCE_LIMIT: return EMFILE;
    case HL_STATUS_NOT_FOUND: return ENOENT;
    case HL_STATUS_ALREADY_EXISTS: return EEXIST;
    case HL_STATUS_PERMISSION_DENIED: return EACCES;
    case HL_STATUS_WOULD_BLOCK: return EAGAIN;
    case HL_STATUS_INTERRUPTED: return EINTR;
    case HL_STATUS_BUSY: return EBUSY;
    case HL_STATUS_NOT_DIRECTORY: return ENOTDIR;
    case HL_STATUS_IS_DIRECTORY: return EISDIR;
    case HL_STATUS_NAME_TOO_LONG: return ENAMETOOLONG;
    case HL_STATUS_SYMLINK_LOOP: return ELOOP;
    case HL_STATUS_READ_ONLY: return EROFS;
    case HL_STATUS_DISCONNECTED: return EPIPE;
    case HL_STATUS_CROSS_DEVICE: return EXDEV;
    case HL_STATUS_NOT_EMPTY: return ENOTEMPTY;
    case HL_STATUS_NO_SPACE: return ENOSPC;
    case HL_STATUS_FILE_TOO_LARGE: return EFBIG;
    case HL_STATUS_TIMED_OUT: return ETIMEDOUT;
    default: return EIO;
    }
}
#endif /* _WIN32 */

#endif
