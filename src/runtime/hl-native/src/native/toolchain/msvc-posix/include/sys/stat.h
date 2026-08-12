/*
 * <sys/stat.h> for the x86_64-pc-windows-msvc target.
 *
 * The UCRT ships this header and its struct stat, so the only gap is the POSIX
 * file-type vocabulary. The UCRT defines _S_IFMT/_S_IFDIR/_S_IFREG/_S_IFCHR and
 * their non-underscore aliases, but none of the S_IS*() classifier macros and
 * none of the type bits Windows has no files for.
 *
 * The values below are the Linux ones, which is also what mingw-w64 uses. That
 * is not a coincidence to be relied on quietly: this tree hands host st_mode
 * bits to guest-visible code paths, so the numbers have to be the guest's.
 * S_IFLNK, S_IFSOCK, S_IFIFO and S_IFBLK name shapes the UCRT's stat() never
 * reports -- it has no encoding for them -- so the corresponding S_IS*() are
 * always false against a UCRT struct stat. They are defined anyway because the
 * classifiers are also applied to modes the engine synthesises itself, where
 * the answer is meaningful.
 */

#ifndef HL_MSVC_POSIX_SYS_STAT_H
#define HL_MSVC_POSIX_SYS_STAT_H

#include <sys/types.h>

#include_next <sys/stat.h>

#ifndef S_IFMT
#define S_IFMT 0170000
#endif
#ifndef S_IFDIR
#define S_IFDIR 0040000
#endif
#ifndef S_IFCHR
#define S_IFCHR 0020000
#endif
#ifndef S_IFREG
#define S_IFREG 0100000
#endif
#ifndef S_IFIFO
#define S_IFIFO 0010000
#endif
#ifndef S_IFBLK
#define S_IFBLK 0060000
#endif
#ifndef S_IFLNK
#define S_IFLNK 0120000
#endif
#ifndef S_IFSOCK
#define S_IFSOCK 0140000
#endif

#define S_ISDIR(m) (((m) & S_IFMT) == S_IFDIR)
#define S_ISREG(m) (((m) & S_IFMT) == S_IFREG)
#define S_ISCHR(m) (((m) & S_IFMT) == S_IFCHR)
#define S_ISBLK(m) (((m) & S_IFMT) == S_IFBLK)
#define S_ISFIFO(m) (((m) & S_IFMT) == S_IFIFO)
#define S_ISLNK(m) (((m) & S_IFMT) == S_IFLNK)
#define S_ISSOCK(m) (((m) & S_IFMT) == S_IFSOCK)

/* Permission bits. The UCRT defines only _S_IREAD/_S_IWRITE/_S_IEXEC (owner),
 * because its file model has no group or other. The group/other bits below are
 * the Linux values and exist so that a mode literal written for POSIX still
 * compiles and still means the same number. */
#ifndef S_IRWXU
#define S_IRWXU 0000700
#endif
#ifndef S_IRUSR
#define S_IRUSR 0000400
#endif
#ifndef S_IWUSR
#define S_IWUSR 0000200
#endif
#ifndef S_IXUSR
#define S_IXUSR 0000100
#endif
#ifndef S_IRWXG
#define S_IRWXG 0000070
#endif
#ifndef S_IRGRP
#define S_IRGRP 0000040
#endif
#ifndef S_IWGRP
#define S_IWGRP 0000020
#endif
#ifndef S_IXGRP
#define S_IXGRP 0000010
#endif
#ifndef S_IRWXO
#define S_IRWXO 0000007
#endif
#ifndef S_IROTH
#define S_IROTH 0000004
#endif
#ifndef S_IWOTH
#define S_IWOTH 0000002
#endif
#ifndef S_IXOTH
#define S_IXOTH 0000001
#endif
#ifndef S_ISUID
#define S_ISUID 0004000
#endif
#ifndef S_ISGID
#define S_ISGID 0002000
#endif
#ifndef S_ISVTX
#define S_ISVTX 0001000
#endif

#endif /* HL_MSVC_POSIX_SYS_STAT_H */
