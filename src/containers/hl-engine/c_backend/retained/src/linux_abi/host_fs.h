#ifndef HL_LINUX_ABI_HOST_FS_H
#define HL_LINUX_ABI_HOST_FS_H

/*
 * <sys/xattr.h> + <sys/mount.h> for this layer -- the filesystem vocabulary
 * that is neither open/stat (the CRT has those) nor mmap (host_mman.h has
 * that): extended attributes, and the filesystem-level statfs/mount pair.
 *
 * Same construction and the same REAL/SHAPE/REFUSAL labelling as host_mman.h.
 * On Linux and macOS this is the system headers; on Windows the vocabulary is
 * synthesized below.
 *
 * TWO BOUNDARIES THIS FILE DELIBERATELY DOES NOT CROSS.
 *
 * (1) src/host/native_compat.h already owns the xattr CALLS.  Its Windows arm
 *     defines the whole hl_native_*xattr family as ENOTSUP refusals, plus
 *     XATTR_NOFOLLOW and ENOATTR, and every xattr caller in this tree
 *     (syscall/fs.c's guest_xattr_{set,get,list,remove}, syscall/binding.c,
 *     container/vfs/overlay.c) goes through those wrappers -- not one of them
 *     names a raw setxattr/getxattr.  So this file adds only what is still
 *     missing there: the two Linux flag values, and raw declarations of the
 *     twelve POSIX-shaped entry points for the sake of standing in honestly for
 *     the header it replaces.  XATTR_NOFOLLOW and ENOATTR are NOT redefined
 *     here; native_compat.h's are the ones in force, and a second definition
 *     would either be redundant or a silent disagreement.
 *
 *     Note that native_compat.h's refusal errno is ENOTSUP, not the ENOSYS the
 *     rest of this seam uses, and that is correct rather than an inconsistency:
 *     ENOTSUP is exactly what Linux returns for a filesystem carrying no
 *     extended-attribute support, so a guest receiving it has been told
 *     something true about the filesystem and already knows how to handle it.
 *     The raw declarations below match that choice for the same reason.
 *
 * (2) `mount` in this tree usually means hl's OWN mount model, not the syscall.
 *     container/vfs.c, vfs/resolve.c, vfs/overlay.c and syscall/fs.c's
 *     svc_mount() implement bind/tmpfs/remount against hl's internal volume
 *     table; the guest's mount(2)/umount2(2) are served from that table and
 *     never reach a host mount call, on any host.  The declarations below
 *     therefore have no caller today and exist only so the header is complete
 *     -- they are refusals, and an honest Windows mount(2) is not a small
 *     project (the nearest primitives are SetVolumeMountPoint for a volume and
 *     a directory reparse point for a bind, neither of which has Linux's
 *     per-process mount-namespace semantics).
 *
 * WHAT IS ACTUALLY LOAD-BEARING HERE is `struct statfs`.  syscall/fs.c's
 * statfs(2)/fstatfs(2) arm declares one by value, fills it from the host call
 * and then re-encodes it into the guest's Linux layout, reading f_bsize,
 * f_blocks, f_bfree, f_bavail, f_files, f_ffree and f_fsid by name.  So the
 * type must be complete, and one field spelling is dictated by the call site
 * rather than by Linux: fs.c reaches f_fsid through `.__val[]` only under
 * `#if defined(__linux__)` and through `.val[]` everywhere else, so the member
 * below is `val` -- the BSD spelling over the Linux layout.
 *
 * struct statvfs and statvfs()/fstatvfs() are NOT here: nothing in this tree
 * names them.  The guest's statfs is encoded from `struct statfs` above and its
 * f_flags word is synthesized from hl's own mount model, not read from a host
 * statvfs.
 */

#if !defined(_WIN32)

#include <sys/mount.h>
#include <sys/xattr.h>
#if defined(__linux__)
/* macOS declares struct statfs and statfs()/fstatfs() from <sys/mount.h>;
 * glibc puts them in <sys/statfs.h> instead.  Pulling it in here is what makes
 * the two non-Windows arms of this seam actually equivalent. */
#include <sys/statfs.h>
#endif

#else /* Windows */

#include <errno.h>
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

/* ---- SHAPE: setxattr flags.  Linux values.  syscall/fs.c currently tests
 * these as the literals 1 and 2 while naming them in its comments; the names
 * are here so a caller that spells them out gets the same numbers. ------- */
#define XATTR_CREATE 0x1
#define XATTR_REPLACE 0x2

/*
 * REFUSAL, whole family.  The reasoning is native_compat.h's and is not
 * repeated in full: NTFS has an EA stream, but it is unreachable from the CRT,
 * its names are upper-cased and capped at 254 bytes, the whole set must fit one
 * buffer, and there is no user./trusted./security. prefix rule for a guest to
 * key off; alternate data streams are a different object entirely (a stream is
 * a file, unbounded, enumerated by a different API, and it survives a copy
 * under different rules).  ENOTSUP rather than ENOSYS, matching the wrappers --
 * it is what Linux itself returns for a filesystem without xattr support.
 *
 * The f* forms would additionally need a host descriptor, which is the same
 * missing descriptor-to-HANDLE table host_mman.h refuses file-backed mmap over.
 */
static inline int setxattr(const char *path, const char *name, const void *value, size_t size, int flags) {
    (void)path;
    (void)name;
    (void)value;
    (void)size;
    (void)flags;
    errno = ENOTSUP;
    return -1;
}

static inline int lsetxattr(const char *path, const char *name, const void *value, size_t size, int flags) {
    (void)path;
    (void)name;
    (void)value;
    (void)size;
    (void)flags;
    errno = ENOTSUP;
    return -1;
}

static inline int fsetxattr(int descriptor, const char *name, const void *value, size_t size, int flags) {
    (void)descriptor;
    (void)name;
    (void)value;
    (void)size;
    (void)flags;
    errno = ENOTSUP;
    return -1;
}

static inline ssize_t getxattr(const char *path, const char *name, void *value, size_t size) {
    (void)path;
    (void)name;
    (void)value;
    (void)size;
    errno = ENOTSUP;
    return -1;
}

static inline ssize_t lgetxattr(const char *path, const char *name, void *value, size_t size) {
    (void)path;
    (void)name;
    (void)value;
    (void)size;
    errno = ENOTSUP;
    return -1;
}

static inline ssize_t fgetxattr(int descriptor, const char *name, void *value, size_t size) {
    (void)descriptor;
    (void)name;
    (void)value;
    (void)size;
    errno = ENOTSUP;
    return -1;
}

/* A listing is the one place where a quiet success is tempting and worst: 0
 * means "this file has no extended attributes", which a guest acts on by
 * deciding there is nothing to copy, nothing to preserve and no capability set
 * to carry.  That is a claim about the file, and it would be unfounded. */
static inline ssize_t listxattr(const char *path, char *list, size_t size) {
    (void)path;
    (void)list;
    (void)size;
    errno = ENOTSUP;
    return -1;
}

static inline ssize_t llistxattr(const char *path, char *list, size_t size) {
    (void)path;
    (void)list;
    (void)size;
    errno = ENOTSUP;
    return -1;
}

static inline ssize_t flistxattr(int descriptor, char *list, size_t size) {
    (void)descriptor;
    (void)list;
    (void)size;
    errno = ENOTSUP;
    return -1;
}

static inline int removexattr(const char *path, const char *name) {
    (void)path;
    (void)name;
    errno = ENOTSUP;
    return -1;
}

static inline int lremovexattr(const char *path, const char *name) {
    (void)path;
    (void)name;
    errno = ENOTSUP;
    return -1;
}

static inline int fremovexattr(int descriptor, const char *name) {
    (void)descriptor;
    (void)name;
    errno = ENOTSUP;
    return -1;
}

/*
 * SHAPE.  struct statfs in the Linux x86-64 layout: the __fsword_t fields are
 * 64 bits there, so they are spelled with an explicit int64_t here rather than
 * `long` -- this target is LLP64 and `long` would silently halve every one of
 * them, changing both the size of the struct and the offset of every field
 * after the first.
 *
 * f_fsid is two 32-bit words, as on Linux, but the member is `val` and not
 * `__val`: syscall/fs.c selects the `__val` spelling only under
 * `#if defined(__linux__)`, so the non-Linux arm it compiles here needs `val`.
 * fsid_t is a typedef of its own because that is how every host that has one
 * declares it, and a caller may name the type.
 */
typedef struct {
    int val[2];
} fsid_t;

struct statfs {
    int64_t f_type;
    int64_t f_bsize;
    uint64_t f_blocks;
    uint64_t f_bfree;
    uint64_t f_bavail;
    uint64_t f_files;
    uint64_t f_ffree;
    fsid_t f_fsid;
    int64_t f_namelen;
    int64_t f_frsize;
    int64_t f_flags;
    int64_t f_spare[4];
};

/*
 * REFUSAL.  There is no host statfs behind this, and the two ways to fake one
 * are both worse than a failure:
 *
 *   - Zeroing the struct reports a filesystem with zero total blocks and zero
 *     free blocks.  `df` prints it, an installer's free-space precheck fails,
 *     and a guest that divides by f_bsize divides by zero.
 *   - Inventing plausible geometry reports free space that does not exist, and
 *     the guest discovers that by running out of it mid-write.
 *
 * The information does exist on this host -- GetDiskFreeSpaceExW for the
 * geometry, GetVolumeInformationW for the name limit and the flags -- so this
 * is a refusal awaiting a Windows file-status path, not an impossibility.  Note
 * that even then f_type would have to be synthesized: Linux's magic numbers
 * name Linux filesystems and NTFS has none, and syscall/fs.c already overrides
 * f_type from hl's own mount classification for every path it can classify.
 */
static inline int statfs(const char *path, struct statfs *buffer) {
    (void)path;
    (void)buffer;
    errno = ENOSYS;
    return -1;
}

static inline int fstatfs(int descriptor, struct statfs *buffer) {
    (void)descriptor;
    (void)buffer;
    errno = ENOSYS;
    return -1;
}

/* ---- SHAPE: mount(2) flags.  Linux values. ------------------------------ */
#define MS_RDONLY 1
#define MS_NOSUID 2
#define MS_NODEV 4
#define MS_NOEXEC 8
#define MS_SYNCHRONOUS 16
#define MS_REMOUNT 32
#define MS_MANDLOCK 64
#define MS_DIRSYNC 128
#define MS_NOATIME 1024
#define MS_NODIRATIME 2048
#define MS_BIND 4096
#define MS_MOVE 8192
#define MS_REC 16384
#define MS_SILENT 32768
#define MS_POSIXACL (1 << 16)
#define MS_UNBINDABLE (1 << 17)
#define MS_PRIVATE (1 << 18)
#define MS_SLAVE (1 << 19)
#define MS_SHARED (1 << 20)
#define MS_RELATIME (1 << 21)
#define MS_KERNMOUNT (1 << 22)
#define MS_I_VERSION (1 << 23)
#define MS_STRICTATIME (1 << 24)
#define MS_LAZYTIME (1 << 25)

/* ---- SHAPE: umount2(2) flags.  Linux values. ---------------------------- */
#define MNT_FORCE 1
#define MNT_DETACH 2
#define MNT_EXPIRE 4
#define UMOUNT_NOFOLLOW 8

/*
 * REFUSAL.  See boundary (2) in the header note: the guest's mount(2) and
 * umount2(2) are served entirely from hl's own volume table and never reach a
 * host mount on ANY host, so these have no caller to disappoint.  They refuse
 * rather than return 0 because a successful mount that mounted nothing is the
 * exact failure syscall/fs.c's own comment records having already been fixed
 * once ("a real no-op stub silently gave wrong content + unenforced RO").
 */
static inline int mount(const char *source, const char *target, const char *filesystem, unsigned long flags,
                        const void *data) {
    (void)source;
    (void)target;
    (void)filesystem;
    (void)flags;
    (void)data;
    errno = ENOSYS;
    return -1;
}

static inline int umount(const char *target) {
    (void)target;
    errno = ENOSYS;
    return -1;
}

static inline int umount2(const char *target, int flags) {
    (void)target;
    (void)flags;
    errno = ENOSYS;
    return -1;
}

#endif /* _WIN32 */

#endif
