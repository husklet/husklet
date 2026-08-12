#ifndef HL_LINUX_ABI_HOST_STAT_H
#define HL_LINUX_ABI_HOST_STAT_H

/*
 * The four `struct stat` members this layer reads that are not on every host's
 * structure, reached through macros instead of through field syntax.
 *
 * This is a different kind of seam from host_mman.h or host_socket.h and the
 * difference is worth stating, because it explains why it is macros and not a
 * replacement type.  Those headers stand in for a header the host does not
 * have.  Here the host HAS <sys/stat.h>, and a real stat() fills a real
 * structure with real values -- there is simply no member for three of the
 * quantities Linux reports.  A synthesized `struct stat` would therefore be
 * strictly worse: every stat/fstat/lstat/fstatat in the CRT fills the CRT's
 * structure, so this layer would have to copy field by field into its own, and
 * the fields that do not exist would still not exist.
 *
 * The tree already accepts this shape.  src/host/native_compat.h's Linux arm
 * spells the same idea as `#define st_atimespec st_atim`, reconciling glibc's
 * member name with Darwin's, which is this tree's lingua franca.  What that
 * trick cannot do is invent a member where the structure has none, which is
 * where these macros start.
 *
 * On Windows the mingw-w64 CRT's `struct stat` carries st_dev, st_ino, st_mode,
 * st_nlink, st_uid, st_gid, st_rdev, st_size and the three time_t stamps -- no
 * sub-second precision, no allocated-block count, no preferred block size.
 * Each substitute below says what it is derived from, so a reader never has to
 * guess whether a zero is a measurement or a placeholder:
 *
 *   - the nanosecond fields are 0.  That is not a placeholder, it is the
 *     precision the value actually has: this CRT reports whole seconds.  A
 *     fabricated sub-second component would make a guest's timestamp
 *     comparisons non-monotonic against its own writes.
 *   - st_blocks is derived from st_size, rounded up to 512-byte units, which is
 *     what a file with no holes occupies.  A sparse file over-reports, and that
 *     is the safe direction: a guest that sizes a copy buffer or a quota check
 *     from it over-allocates rather than truncating.  du(1) over a sparse file
 *     is the one visible consequence.
 *   - st_blksize is the guest's page size rather than the volume's cluster
 *     size.  It is advisory -- an I/O granularity hint -- and every caller in
 *     this layer uses it that way; querying the real cluster size needs a
 *     volume path this layer does not hold at the point of the stat.
 */

#if defined(_WIN32)

#include <stdint.h>
#include <sys/stat.h>

#define HL_HOST_STAT_ATIME_SEC(s) ((int64_t)(s)->st_atime)
#define HL_HOST_STAT_MTIME_SEC(s) ((int64_t)(s)->st_mtime)
#define HL_HOST_STAT_CTIME_SEC(s) ((int64_t)(s)->st_ctime)
#define HL_HOST_STAT_ATIME_NSEC(s) ((void)(s), UINT64_C(0))
#define HL_HOST_STAT_MTIME_NSEC(s) ((void)(s), UINT64_C(0))
#define HL_HOST_STAT_CTIME_NSEC(s) ((void)(s), UINT64_C(0))
#define HL_HOST_STAT_BLOCKS(s) (((uint64_t)(s)->st_size + UINT64_C(511)) / UINT64_C(512))
#define HL_HOST_STAT_BLKSIZE(s) ((void)(s), UINT64_C(4096))

/* The setters exist because two sites do not stat a file at all -- they FILL a
 * struct stat from metadata that came from somewhere else (a provider's typed
 * file status, a RAM-backed file's own size) and then hand it to the same
 * encoder.  Those sites have a real block count and a real nanosecond stamp,
 * and this host's structure has nowhere to put them; on Windows the seconds
 * land and the rest is dropped, and the getters above then re-derive what they
 * can.  The one visible consequence is that a provider-reported sparse block
 * count is replaced by the size-derived one on the way back out.
 *
 * Written as macros taking the whole value so a caller reads as an assignment,
 * and so the discarded arguments are still evaluated exactly once -- several of
 * them are divisions of a nanosecond stamp, not constants. */
#define HL_HOST_STAT_SET_BLOCKS(s, v) ((void)(s), (void)(v))
#define HL_HOST_STAT_SET_BLKSIZE(s, v) ((void)(s), (void)(v))
#define HL_HOST_STAT_SET_ATIME(s, sec, nsec) ((s)->st_atime = (time_t)(sec), (void)(nsec))
#define HL_HOST_STAT_SET_MTIME(s, sec, nsec) ((s)->st_mtime = (time_t)(sec), (void)(nsec))
#define HL_HOST_STAT_SET_CTIME(s, sec, nsec) ((s)->st_ctime = (time_t)(sec), (void)(nsec))

#else

#include <stdint.h>
#include <sys/stat.h>

/* st_atimespec is Darwin's name natively and glibc's st_atim under the alias
 * native_compat.h installs, so one spelling serves both POSIX arms. */
#define HL_HOST_STAT_ATIME_SEC(s) ((int64_t)(s)->st_atimespec.tv_sec)
#define HL_HOST_STAT_MTIME_SEC(s) ((int64_t)(s)->st_mtimespec.tv_sec)
#define HL_HOST_STAT_CTIME_SEC(s) ((int64_t)(s)->st_ctimespec.tv_sec)
#define HL_HOST_STAT_ATIME_NSEC(s) ((uint64_t)(s)->st_atimespec.tv_nsec)
#define HL_HOST_STAT_MTIME_NSEC(s) ((uint64_t)(s)->st_mtimespec.tv_nsec)
#define HL_HOST_STAT_CTIME_NSEC(s) ((uint64_t)(s)->st_ctimespec.tv_nsec)
#define HL_HOST_STAT_BLOCKS(s) ((uint64_t)(s)->st_blocks)
#define HL_HOST_STAT_BLKSIZE(s) ((uint64_t)(s)->st_blksize)

#define HL_HOST_STAT_SET_BLOCKS(s, v) ((s)->st_blocks = (blkcnt_t)(v))
#define HL_HOST_STAT_SET_BLKSIZE(s, v) ((s)->st_blksize = (blksize_t)(v))
#define HL_HOST_STAT_SET_ATIME(s, sec, nsec)                                                                           \
    ((s)->st_atimespec.tv_sec = (time_t)(sec), (s)->st_atimespec.tv_nsec = (long)(nsec))
#define HL_HOST_STAT_SET_MTIME(s, sec, nsec)                                                                           \
    ((s)->st_mtimespec.tv_sec = (time_t)(sec), (s)->st_mtimespec.tv_nsec = (long)(nsec))
#define HL_HOST_STAT_SET_CTIME(s, sec, nsec)                                                                           \
    ((s)->st_ctimespec.tv_sec = (time_t)(sec), (s)->st_ctimespec.tv_nsec = (long)(nsec))

#endif

#endif
