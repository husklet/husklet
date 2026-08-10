#ifndef HL_LINUX_ABI_HOST_MMAN_H
#define HL_LINUX_ABI_HOST_MMAN_H

/*
 * <sys/mman.h> for this layer.
 *
 * On Linux and macOS this is the system header and nothing else, so the
 * preprocessor input on those hosts is exactly what it was before this file
 * existed.  On Windows there is no <sys/mman.h> at all, and the vocabulary has
 * to come from somewhere or the guest-target unity TU does not survive its own
 * include list.
 *
 * src/host/native_compat.h is deliberately NOT that somewhere -- it says so in
 * its own Windows arm: the POSIX descriptor and VM vocabulary is what the other
 * two arms get from system headers, and inventing it there would be a new layer
 * wearing that one's name.  It belongs here instead, in the layer that actually
 * calls it, next to the guest ABI it is being reconciled against.
 *
 * Three kinds of entry, labelled at each one, using the same words the
 * native_compat.h Windows arm uses so the two read alike:
 *
 *   REAL     -- routed through the typed host seam (hl_host_services).  Same
 *               operation, one call deeper.
 *   SHAPE    -- a constant or type with no behaviour of its own.  Present so
 *               callers compile.  Values are the Linux ones on purpose: this
 *               layer passes a guest protection/flag word straight into a host
 *               mmap in several places, and that identity is what makes those
 *               sites correct on Linux today.
 *   REFUSAL  -- returns failure with a specific errno and never a quiet
 *               success.  Each one names what is missing, and none of them is a
 *               constant chosen to let a call compile and then do something
 *               other than what its name says.
 *
 * The refusals here fall into exactly two populations and it is worth being
 * precise about which, because they have different fixes:
 *
 *   (1) No address-keyed operation exists in the memory group.  protect(),
 *       sync() and unmap_range() are all keyed on an ownership handle, and this
 *       layer holds an address.  unmap_address() was appended at memory ABI 7
 *       for exactly this reason and munmap() below uses it; the same argument
 *       applies unchanged to mprotect() and msync(), which have no such
 *       appended twin yet.  That is a seam gap, not a Windows one -- a
 *       protect_address()/sync_address() pair would make both REAL.
 *
 *   (2) The operation needs a host descriptor.  A file-backed mmap, shm_open
 *       and memfd_create all start from an int fd, and on Windows a guest fd
 *       number is not a host object -- the engine has no descriptor-to-HANDLE
 *       table yet.  These stay refusals until it does.
 *
 * INCLUDE ONLY FROM THE GUEST-TARGET UNITY TU (or a header it pulls in).  The
 * Windows arm reaches the seam through effective_host_services(), which is a
 * static defined in src/core/target/{x86_64,aarch64}.c, so a file compiled
 * standalone into libhl-linux-abi has no way to satisfy it.  The library
 * sources that call raw mmap -- fdcache.c, logical_vma.c, container/vfs/gmap.c
 * -- therefore keep their own `#if !defined(_WIN32)` guards around the system
 * header and are deliberately NOT routed here; each of them is a separate,
 * smaller port of its own.  Nothing enforces this mechanically, but a violation
 * fails loudly at compile time ("static declared but never defined") rather
 * than silently.
 */

#if !defined(_WIN32)

#include <sys/mman.h>

#else /* Windows */

#include "fdhandle.h"
#include "host_fd.h" /* openat()/unlink()/AT_FDCWD/O_CLOEXEC for the shm backing below */
#include "hl/host_services.h"

#include <direct.h>
#include <errno.h>
#include <fcntl.h>
#include <io.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/types.h>

/* Defined later in the same target TU (src/core/target/{x86_64,aarch64}.c).
 * Declaring a static function ahead of its definition is what src/linux_abi/
 * elf_protect.h already relies on for the same accessor. */
static const hl_host_services *effective_host_services(void);

/* Defined later in the same TU by src/host/native_compat.h, which the target
 * unit includes after this file.  Same forward-static arrangement, and for the
 * same reason: this header must not include that one (they are different layers
 * and native_compat.h says so in its own note), but the operation it needs --
 * "the path of an already-open descriptor" -- is REAL there on this host and
 * exists nowhere else. */
static int hl_native_fd_path(int descriptor, char *path, size_t capacity);

/* ---- SHAPE: protection bits.  Linux values. ---------------------------- */
#define PROT_NONE 0x0
#define PROT_READ 0x1
#define PROT_WRITE 0x2
#define PROT_EXEC 0x4
#define PROT_GROWSDOWN 0x01000000
#define PROT_GROWSUP 0x02000000

/* ---- SHAPE: mmap flags.  Linux/x86-64 values. -------------------------- */
#define MAP_SHARED 0x01
#define MAP_PRIVATE 0x02
#define MAP_SHARED_VALIDATE 0x03
#define MAP_FIXED 0x10
#define MAP_ANONYMOUS 0x20
#define MAP_ANON MAP_ANONYMOUS
#define MAP_32BIT 0x40
#define MAP_NORESERVE 0x4000
#define MAP_POPULATE 0x8000
#define MAP_HUGETLB 0x40000
#define MAP_FIXED_NOREPLACE 0x100000
#define MAP_FAILED ((void *)-1)

/* ---- SHAPE: msync / madvise / mlockall / mremap / memfd words. --------- */
#define MS_ASYNC 1
#define MS_INVALIDATE 2
#define MS_SYNC 4

#define MADV_NORMAL 0
#define MADV_RANDOM 1
#define MADV_SEQUENTIAL 2
#define MADV_WILLNEED 3
#define MADV_DONTNEED 4
#define MADV_FREE 8
#define MADV_REMOVE 9
#define MADV_DONTFORK 10
#define MADV_DOFORK 11
#define MADV_WIPEONFORK 18
#define MADV_KEEPONFORK 19

#define MCL_CURRENT 1
#define MCL_FUTURE 2
#define MCL_ONFAULT 4

#define MREMAP_MAYMOVE 1
#define MREMAP_FIXED 2
#define MREMAP_DONTUNMAP 4

#define MFD_CLOEXEC 0x0001U
#define MFD_ALLOW_SEALING 0x0002U
#define MFD_HUGETLB 0x0004U
#define MFD_NOEXEC_SEAL 0x0008U
#define MFD_EXEC 0x0010U

/* Map a POSIX protection word onto the seam's. Windows has no write-without-read
 * page protection, but neither does the x86-64 hardware Linux runs this on, so
 * PROT_WRITE alone already widens to read+write everywhere and nothing observes
 * the difference here. */
static inline uint32_t hl_linux_host_protection(int protection) {
    uint32_t translated = 0;
    if (protection & PROT_READ) translated |= HL_HOST_MEMORY_READ;
    if (protection & PROT_WRITE) translated |= HL_HOST_MEMORY_WRITE;
    if (protection & PROT_EXEC) translated |= HL_HOST_MEMORY_EXECUTE;
    return translated;
}

/*
 * REAL (anonymous) / REFUSAL (file-backed).
 *
 * The anonymous case is map_anonymous() followed immediately by discard().
 * That composition is deliberate and is not a leak: map_anonymous() hands back
 * an ownership handle, discard() retires the handle without touching the
 * address space, and what remains is exactly what mmap(2) leaves behind -- live
 * pages that no ownership handle covers.  It is also what makes munmap() below
 * work, because unmap_address() refuses any range overlapping a LIVE handle
 * (HL_STATUS_BUSY) and would otherwise reject every range this function
 * produced.  Retaining the handle instead would be the wrong trade: the caller
 * has nowhere to put it, and a handle left live over memory its owner later
 * carves a hole in is the failure mode the memory group's own notes warn about.
 *
 * A file-backed request needs map_file(), which takes an hl_host_handle for the
 * file.  The caller has an int fd -- so this used to be population (2)'s
 * refusal, "the engine has no descriptor-to-HANDLE table yet".  It has one now
 * (fdhandle.h), and a BOUND descriptor is therefore REAL through map_file(),
 * which is the same operation down to the offset and the sharing flag.
 *
 * AN UNBOUND DESCRIPTOR IS NOW REAL TOO, through a transient handle, and the
 * reason it is sound is specific rather than general.  On this host the
 * overwhelmingly common descriptor is the UCRT's own -- host_fd.h's openat()
 * produces one for every guest open and files only an AMBIENT binding, which
 * carries state and no handle.  Refusing those meant no guest could mmap a file
 * it had opened, which is most of what a file-backed mmap is for.  So a miss
 * asks the host for the descriptor's PATH (hl_native_fd_path, which is
 * GetFinalPathNameByHandleW walking the open file object back to a name) and
 * opens that path through the file group for the duration of the call.
 *
 * The transient handle is closed as soon as map_file returns, and that is not a
 * lifetime shortcut -- it is the same fact the discard() note below rests on.
 * MapViewOfFile3 has already succeeded by then; the view holds the section and
 * the section holds the file, so neither this handle nor the caller's descriptor
 * is load-bearing afterwards.  Nothing is left open and nothing leaks.
 *
 * Two honest limits.  Re-opening by name is not the same act as mapping the
 * descriptor: a file that has been unlinked, or whose name this process may no
 * longer open, is refused here where a real mmap would succeed.  And a
 * descriptor with no name at all -- a pipe, a console -- has no path to reopen,
 * which is correct, because neither is mappable.
 *
 * The file case ends in the same discard() as the anonymous one and for the
 * same reason: munmap() below is address-keyed, unmap_address() refuses any
 * range overlapping a live ownership handle, and mmap(2)'s contract is that the
 * caller gets pages and not a token.  discard() on a file mapping releases the
 * section this layer never sees and leaves the view exactly where it is.
 */
static inline void *hl_linux_host_map_file(void *address, size_t length, int protection, int flags, int fd,
                                           off_t offset) {
    const hl_host_services *host = effective_host_services();
    hl_host_file_mapping mapping = {HL_HOST_FILE_MAPPING_ABI, sizeof(mapping), 0, 0, 0, 0};
    hl_host_result result;
    hl_host_handle file;
    int transient = 0;
    uint32_t seam_flags = (flags & MAP_SHARED) ? HL_HOST_MEMORY_SHARED : HL_HOST_MEMORY_PRIVATE;
    if (host == NULL || host->memory == NULL || host->memory->map_file == NULL) {
        errno = ENOSYS;
        return MAP_FAILED;
    }
    if (length == 0 || offset < 0) {
        errno = EINVAL;
        return MAP_FAILED;
    }
    if (!hl_fdhandle_lookup(fd, &file)) {
        /* Ambient descriptor: reopen its name for the duration of the call. */
        char path[4096];
        uint32_t access = HL_HOST_FILE_READ;
        if ((protection & PROT_WRITE) && (flags & MAP_SHARED)) access |= HL_HOST_FILE_WRITE;
        if (host->file == NULL || host->file->open_relative == NULL || host->file->close == NULL ||
            hl_native_fd_path(fd, path, sizeof path) != 0) {
            errno = ENOSYS;
            return MAP_FAILED;
        }
        result = host->file->open_relative(host->context, HL_HOST_HANDLE_CWD, path, strlen(path), access, 0, 0);
        if (result.status != HL_STATUS_OK) {
            errno = hl_fdhandle_errno(result.status);
            return MAP_FAILED;
        }
        file = result.value;
        transient = 1;
    }
    if (flags & MAP_FIXED_NOREPLACE)
        seam_flags |= HL_HOST_MEMORY_FIXED_NOREPLACE;
    else if (flags & MAP_FIXED)
        seam_flags |= HL_HOST_MEMORY_FIXED;
    result = host->memory->map_file(host->context, file, (uint64_t)(uintptr_t)address, (uint64_t)offset,
                                    (uint64_t)length, hl_linux_host_protection(protection), seam_flags, &mapping);
    /* The view now owns everything it needs; see the note above. */
    if (transient) (void)host->file->close(host->context, file);
    if (result.status != HL_STATUS_OK) {
        errno = hl_fdhandle_errno(result.status);
        return MAP_FAILED;
    }
    /* Ownership retired, pages kept: see the note above. */
    if (host->memory->discard != NULL) (void)host->memory->discard(host->context, mapping.handle);
    return (void *)(uintptr_t)mapping.address;
}

static inline void *mmap(void *address, size_t length, int protection, int flags, int fd, off_t offset) {
    const hl_host_services *host = effective_host_services();
    hl_host_memory_mapping mapping = {HL_HOST_MEMORY_MAPPING_ABI, sizeof(mapping), 0, 0, 0, 0};
    hl_host_result result;
    uint32_t seam_flags = (flags & MAP_SHARED) ? HL_HOST_MEMORY_SHARED : HL_HOST_MEMORY_PRIVATE;

    if (!(flags & MAP_ANONYMOUS) || fd >= 0)
        return hl_linux_host_map_file(address, length, protection, flags, fd, offset);
    if (host == NULL || host->memory == NULL || length == 0) {
        errno = EINVAL;
        return MAP_FAILED;
    }
    (void)offset;
    if (flags & MAP_FIXED_NOREPLACE)
        seam_flags |= HL_HOST_MEMORY_FIXED_NOREPLACE;
    else if (flags & MAP_FIXED)
        seam_flags |= HL_HOST_MEMORY_FIXED;

    result = host->memory->map_anonymous(host->context, (uint64_t)(uintptr_t)address, (uint64_t)length,
                                         hl_linux_host_protection(protection), seam_flags, &mapping);
    if (result.status != HL_STATUS_OK) {
        errno = (result.status == HL_STATUS_OUT_OF_MEMORY) ? ENOMEM : EINVAL;
        return MAP_FAILED;
    }
    /* Ownership retired, pages kept: see the note above. */
    (void)host->memory->discard(host->context, mapping.handle);
    return (void *)(uintptr_t)mapping.address;
}

/*
 * REAL.  unmap_address() is the address-keyed release the memory group grew at
 * ABI 7, and its contract is the one this layer needs: a range overlapping any
 * live ownership handle is refused whole with HL_STATUS_BUSY and nothing is
 * unmapped, and a range with no mapping at all succeeds -- which is munmap(2)'s
 * own behaviour for an already-unmapped range, and is what the hole-carving in
 * syscall/mem.c relies on.
 */
static inline int munmap(void *address, size_t length) {
    const hl_host_services *host = effective_host_services();
    hl_host_result result;
    if (host == NULL || host->memory == NULL || host->memory->unmap_address == NULL) {
        errno = ENOSYS;
        return -1;
    }
    result = host->memory->unmap_address(host->context, (uint64_t)(uintptr_t)address, (uint64_t)length);
    if (result.status == HL_STATUS_OK) return 0;
    errno = (result.status == HL_STATUS_BUSY) ? EBUSY : EINVAL;
    return -1;
}

/*
 * REAL.  mlock/munlock are the one pair the seam already answers by address:
 * wire_range()/unwire_range() take (address, size) and were appended for
 * precisely this caller.  The kind the host applied comes back in detail --
 * Windows VirtualLock only grows the working set (HL_HOST_WIRE_WORKING_SET)
 * where Linux mlock pins against reclaim -- and a host with no wiring primitive
 * answers HL_STATUS_NOT_SUPPORTED, which the guest sees as ENOSYS rather than a
 * false success.
 */
static inline int hl_linux_host_wire(void *address, size_t length, int wire) {
    const hl_host_services *host = effective_host_services();
    hl_host_result result;
    if (host == NULL || host->memory == NULL || host->memory->wire_range == NULL) {
        errno = ENOSYS;
        return -1;
    }
    result = wire ? host->memory->wire_range(host->context, (uint64_t)(uintptr_t)address, (uint64_t)length, 0)
                  : host->memory->unwire_range(host->context, (uint64_t)(uintptr_t)address, (uint64_t)length);
    if (result.status == HL_STATUS_OK) return 0;
    errno = (result.status == HL_STATUS_NOT_SUPPORTED) ? ENOSYS : EAGAIN;
    return -1;
}

static inline int mlock(const void *address, size_t length) {
    return hl_linux_host_wire((void *)address, length, 1);
}

static inline int munlock(const void *address, size_t length) {
    return hl_linux_host_wire((void *)address, length, 0);
}

/* REFUSAL -- population (1).  The memory group's protect() is keyed on an
 * ownership handle and this layer holds an address; there is no protect_address()
 * twin to unmap_address() yet.  ENOSYS rather than a silent 0, because a guest
 * mprotect that reports success and does not change the protection turns a
 * would-be SIGSEGV into a completed store. */
static inline int mprotect(void *address, size_t length, int protection) {
    (void)address;
    (void)length;
    (void)protection;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL -- population (1).  Same shape: sync() is handle-keyed. */
static inline int msync(void *address, size_t length, int flags) {
    (void)address;
    (void)length;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL -- no advice operation in the memory group at all, on any host.
 * madvise is advisory on Linux and a caller that treats a failure as fatal is
 * already wrong there, but the two advices this layer relies on for behaviour
 * rather than hints -- MADV_DONTNEED (range must read back zero) and
 * MADV_REMOVE -- are NOT advisory, so answering 0 would be a silent data bug. */
static inline int madvise(void *address, size_t length, int advice) {
    (void)address;
    (void)length;
    (void)advice;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL -- residency query, no seam operation.  Reporting "all resident" or
 * "none resident" are both answers a guest acts on. */
static inline int mincore(void *address, size_t length, unsigned char *vector) {
    (void)address;
    (void)length;
    (void)vector;
    errno = ENOSYS;
    return -1;
}

/* REFUSAL -- remap-and-move has no seam operation and cannot be synthesized
 * from map_anonymous + unmap_address without losing the contents. */
static inline void *mremap(void *address, size_t old_length, size_t new_length, int flags, ...) {
    (void)address;
    (void)old_length;
    (void)new_length;
    (void)flags;
    errno = ENOSYS;
    return MAP_FAILED;
}

/* REFUSAL -- population (2), and the only one of the three that is still in it.
 * memfd_create's whole point is an ANONYMOUS descriptor with no name anywhere in
 * any namespace, which is the one thing the backing below cannot be: a file has
 * a name by construction.  Windows' nearest object, a pagefile-backed section,
 * is also nameless when unnamed -- but it is a section and not a descriptor, and
 * the sealing family (F_ADD_SEALS) that is memfd's reason to exist has no
 * Windows counterpart at all. */
static inline int memfd_create(const char *name, unsigned int flags) {
    (void)name;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

/*
 * The host directory POSIX shared-memory objects are backed by.
 *
 * /dev/shm is the Linux spelling and this host has no such thing, so the
 * subtree is given a real host directory -- exactly the choice
 * src/linux_abi/container/shm.c already made for the /dev/shm the GUEST sees,
 * which it backs with a per-namespace directory under /tmp.  This is the same
 * decision for the objects the ENGINE creates for itself, and keeping the two
 * conventions alike is the point: one place to look, one thing to clear.
 *
 * A single flat directory is not a simplification of the POSIX namespace, it IS
 * the POSIX namespace: shm_open names are flat by specification, which is why
 * the separator folding below can never let one escape.
 */
#define HL_LINUX_SHM_DIRECTORY "/tmp/.hl-ipc"

static inline const char *hl_linux_shm_backing(const char *name, char *output, size_t capacity) {
    size_t index;
    size_t prefix = sizeof(HL_LINUX_SHM_DIRECTORY) - 1u;
    int written;
    if (name == NULL || output == NULL) {
        errno = EINVAL;
        return NULL;
    }
    /* A POSIX shm name is conventionally written with a leading slash, and that
       slash is not a directory step -- "/foo" and "foo" name the same object. */
    if (name[0] == '/') ++name;
    if (name[0] == '\0' || strcmp(name, ".") == 0 || strcmp(name, "..") == 0) {
        errno = EINVAL;
        return NULL;
    }
    /* Idempotent, and attempted on every call rather than once: a caller may
       reach an object before anything has created the directory, and this layer
       has no initialisation point of its own that is guaranteed to run first. */
    (void)_mkdir("/tmp");
    (void)_mkdir(HL_LINUX_SHM_DIRECTORY);
    written = snprintf(output, capacity, "%s/%s", HL_LINUX_SHM_DIRECTORY, name);
    if (written < 0 || (size_t)written >= capacity) {
        errno = ENAMETOOLONG;
        return NULL;
    }
    /* Fold every separator in the NAME part, so no name can name a directory
       above the backing one.  The prefix itself is left alone. */
    for (index = prefix + 1u; output[index] != '\0'; ++index)
        if (output[index] == '/' || output[index] == '\\') output[index] = '_';
    return output;
}

/*
 * REAL, over a file, and the choice of backing is the substance of this entry
 * rather than an implementation detail.
 *
 * The alternative considered and rejected was a NAMED WINDOWS SECTION in a
 * per-session object directory -- which is what a Windows-native shared memory
 * object is, and what a study of flinux's substrate proposes.  It is the wrong
 * primitive here, for a reason that is about lifetime and not about effort:
 *
 *   - A Windows section name lives exactly as long as some handle to the section
 *     is open.  Close the last handle and the name is gone -- even with a view
 *     still mapped, which is the case that matters.  POSIX shm and System V shm
 *     both specify the OPPOSITE: the object outlives every attachment and every
 *     process, and is removed only by shm_unlink / IPC_RMID.  The engine's SysV
 *     emulation depends on that directly (syscall/sysv.c opens the control block
 *     by name from an UNRELATED process, which is the property that makes an id
 *     namespace shared at all), so a section would need some process to hold a
 *     keepalive handle for the lifetime of the namespace -- and there is no such
 *     process, because every guest process is allowed to exit.
 *
 *   - A file already has exactly the lifetime System V wants, on every host, with
 *     no keepalive and no new contract.  Nothing had to be added to
 *     hl_host_shared_memory_services: the file group already opens, truncates,
 *     unlinks and maps, and map_file's shared path over a file HANDLE is already
 *     implemented and already exercised.
 *
 * So the descriptor this returns is the UCRT's own, exactly as host_fd.h's
 * openat() produces, and ftruncate/close/unlink on it are the CRT's -- already
 * REAL on this host.  The mapping is the ambient path in hl_linux_host_map_file
 * above.  There is no second object and no second position anywhere.
 *
 * ONE DIFFERENCE THAT IS NOT SYNTHESIZABLE, stated rather than hidden: Windows
 * refuses to delete a file that still has a live section mapping it, so a
 * shm_unlink issued while another process still has the object MAPPED fails
 * where Linux would succeed and defer.  The engine's own SysV path does not hit
 * this -- it already defers IPC_RMID until nattch reaches zero, which is Linux's
 * own rule -- and the cost when it is hit is a file left behind in the backing
 * directory, not a wrong answer to the guest.
 */
static inline int shm_open(const char *name, int flags, mode_t mode) {
    char path[512];
    if (hl_linux_shm_backing(name, path, sizeof path) == NULL) return -1;
    /* O_CLOEXEC matches glibc's shm_open, which does not leak the object's
       descriptor across an exec; the object itself survives in the namespace. */
    return openat(AT_FDCWD, path, flags | O_CLOEXEC, mode);
}

static inline int shm_unlink(const char *name) {
    char path[512];
    if (hl_linux_shm_backing(name, path, sizeof path) == NULL) return -1;
    return unlink(path);
}

#endif /* _WIN32 */

#endif
