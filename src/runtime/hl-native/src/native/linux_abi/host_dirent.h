#ifndef HL_LINUX_ABI_HOST_DIRENT_H
#define HL_LINUX_ABI_HOST_DIRENT_H

/*
 * <dirent.h> for this layer.  Same construction and the same REAL/SHAPE/REFUSAL
 * labelling as host_mman.h and host_poll.h.
 *
 * Windows is the awkward case here, and not for the usual reason.  mingw-w64
 * DOES ship a <dirent.h> with a working opendir/readdir/closedir over
 * FindFirstFileW, so the temptation is to use it.  It is refused for one
 * measured reason: its `struct dirent` has no d_type.  Every caller in this
 * layer that walks a directory reads d_type -- the /proc emptiness scan, the
 * overlay whiteout and opaque-dir scans, and getdents64, which must PUT a
 * d_type in the guest's buffer because the Linux structure has one.  A host
 * dirent without the field cannot answer, and the fallback every such caller
 * would need (an lstat per entry) is a different implementation, not a smaller
 * one.
 *
 * Adopting mingw's struct and also defining DT_* would compile and then be
 * wrong in the worst available way: `e->d_type` would not exist, so the code
 * would not compile at all -- or, if the field were papered over, every entry
 * would report the same type.  Defining this layer's own Linux-shaped structure
 * keeps the shape honest and makes the absence a refusal at the call rather
 * than a lie in the data.
 *
 * So on Windows: the TYPES and the DT_* constants are SHAPE, with Linux values
 * and the Linux structure, and every call is a REFUSAL.  The refusal is ENOSYS
 * and never an empty directory -- "opendir succeeded and the directory has no
 * entries" is a factual claim about the filesystem that this host cannot make,
 * and a guest that acts on it deletes nothing, finds nothing, and reports
 * success.
 *
 * Making these REAL is a self-contained piece of work with a clear shape: a
 * FindFirstFileW/FindNextFileW walk, d_type derived from
 * dwFileAttributes (DIRECTORY -> DT_DIR, REPARSE_POINT with a symlink tag ->
 * DT_LNK, otherwise DT_REG), and d_ino from the file index.  It needs no
 * descriptor table, which is what distinguishes it from fdopendir below.
 */

#if !defined(_WIN32)

#include <dirent.h>

#else /* Windows */

#include <errno.h>
#include <stddef.h>
#include <stdint.h>

/* SHAPE.  Linux d_type values.  They are handed straight into the guest's
 * dirent64 record by getdents64, so they are the guest's numbers by
 * construction and must not be a host encoding. */
#define DT_UNKNOWN 0
#define DT_FIFO 1
#define DT_CHR 2
#define DT_DIR 4
#define DT_BLK 6
#define DT_REG 8
#define DT_LNK 10
#define DT_SOCK 12
#define DT_WHT 14

/* SHAPE.  The Linux layout, member for member.  d_name is 256 bytes there and
 * this layer copies into it with sizeof, so shortening it would silently
 * truncate names rather than fail. */
struct dirent {
    uint64_t d_ino;
    int64_t d_off;
    unsigned short d_reclen;
    unsigned char d_type;
    char d_name[256];
};

/* SHAPE.  Opaque on every host; this layer only ever holds the pointer. */
typedef struct hl_linux_host_dir DIR;

/* REFUSAL -- see the header note.  Never an empty directory. */
static inline DIR *opendir(const char *path) {
    (void)path;
    errno = ENOSYS;
    return NULL;
}

/*
 * REFUSAL, and the one here that would still be a refusal after the
 * FindFirstFileW walk above exists: fdopendir starts from a descriptor, and on
 * this host a descriptor in this layer is a guest number that names no host
 * object -- the same gap host_mman.h records for a file-backed mmap.  It is
 * listed separately from opendir for exactly that reason.
 */
static inline DIR *fdopendir(int descriptor) {
    (void)descriptor;
    errno = ENOSYS;
    return NULL;
}

/* REFUSAL.  NULL from readdir is indistinguishable from end-of-directory by
 * return value alone, which is why POSIX has callers clear errno first; this
 * sets it so a caller that checks sees ENOSYS rather than a clean end. */
static inline struct dirent *readdir(DIR *directory) {
    (void)directory;
    errno = ENOSYS;
    return NULL;
}

static inline int readdir_r(DIR *directory, struct dirent *entry, struct dirent **result) {
    (void)directory;
    (void)entry;
    (void)result;
    return ENOSYS;
}

static inline int closedir(DIR *directory) {
    (void)directory;
    errno = ENOSYS;
    return -1;
}

static inline void rewinddir(DIR *directory) {
    (void)directory;
}

static inline long telldir(DIR *directory) {
    (void)directory;
    errno = ENOSYS;
    return -1;
}

static inline void seekdir(DIR *directory, long position) {
    (void)directory;
    (void)position;
}

/* REFUSAL.  -1 is the POSIX failure value and is not a valid descriptor, so a
 * caller that passes it on gets EBADF rather than acting on fd 0. */
static inline int dirfd(DIR *directory) {
    (void)directory;
    errno = ENOSYS;
    return -1;
}

#endif /* _WIN32 */

#endif
