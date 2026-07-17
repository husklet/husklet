// #416 LTP fs family — the *at() dirfd/flag/EFAULT error-path fidelity that hl used to miss. Every syscall
// here is a standard kernel path, so the fixed-string verdict is byte-identical on native aarch64 and under
// qemu-x86_64 -> oracle-diffable on both Linux engines. Historical hl gaps this locks down (fs.c):
//   * fstatat/statx/symlinkat/linkat/renameat2/unlinkat folded a bad/non-dir dirfd into an absolute host
//     path via g_fdpath, so EBADF/ENOTDIR were never produced (or leaked macOS EOPNOTSUPP);
//   * fstatat/statx/linkat/renameat2 ignored invalid flag/mask bits (no EINVAL);
//   * statx/unlink read a PROT_NONE guard-page path as an empty string (ENOENT) instead of EFAULT;
//   * a hardlink whose source is on a pseudo-fs (/proc) returned ENOENT instead of EXDEV.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef AT_FDCWD
#define AT_FDCWD -100
#endif
#define RENAME_EXCHANGE 2
#define RENAME_WHITEOUT 4

// raw wrappers (avoid glibc struct/version deps; errno is set on -1)
static long x_statx(int dfd, const char *p, int flags, unsigned mask, void *buf) {
    return syscall(SYS_statx, dfd, p, flags, mask, buf);
}
static long x_renameat2(int od, const char *op, int nd, const char *np, unsigned fl) {
    return syscall(SYS_renameat2, od, op, nd, np, fl);
}

static int fails(long r, int want) { return r == -1 && errno == want; }

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_aterr_%d", (int)getpid());
    mkdir(dir, 0755);
    if (chdir(dir) != 0) { printf("aterr chdir_failed\n"); return 1; }
    int fd = open("file", O_CREAT | O_RDWR, 0644);
    if (fd >= 0) close(fd);
    fd = open("file2", O_CREAT | O_RDWR, 0644);
    if (fd >= 0) close(fd);

    int regfd = open("file", O_RDONLY);        // a regular-file fd (NOT a directory)
    int dirfd = open(".", O_DIRECTORY | O_RDONLY);
    char stbuf[256];
    struct stat st;
    void *bad = mmap(0, 1, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

    // fstatat: non-dir dirfd -> ENOTDIR, bad dirfd -> EBADF, unknown flag -> EINVAL
    int fstatat_enotdir = fails(fstatat(regfd, "x", &st, 0), ENOTDIR);
    int fstatat_ebadf = fails(fstatat(100, "x", &st, 0), EBADF);
    int fstatat_einval = fails(fstatat(dirfd, "x", &st, 9999), EINVAL);

    // statx: bad path ptr -> EFAULT, bad flag -> EINVAL, reserved mask -> EINVAL, non-dir dirfd -> ENOTDIR
    int statx_efault = fails(x_statx(AT_FDCWD, (const char *)bad, 0, 0, stbuf), EFAULT);
    int statx_einval_flag = fails(x_statx(AT_FDCWD, "file", -1, 0, stbuf), EINVAL);
    int statx_einval_mask = fails(x_statx(AT_FDCWD, "file", 0, 0xffffffffu, stbuf), EINVAL);
    int statx_enotdir = fails(x_statx(regfd, "x", 0, 0, stbuf), ENOTDIR);

    // symlinkat / linkat / renameat2 / unlink error paths
    int symlinkat_enotdir = fails(symlinkat("tgt", regfd, "x"), ENOTDIR);
    int linkat_enotdir = fails(linkat(regfd, "a", dirfd, "b", 0), ENOTDIR);
    int linkat_einval = fails(linkat(dirfd, "file", AT_FDCWD, "newlink", 1), EINVAL);
    int linkat_exdev = fails(linkat(AT_FDCWD, "/proc/cpuinfo", dirfd, "b", 0), EXDEV);
    int renameat2_einval =
        fails(x_renameat2(dirfd, "file", dirfd, "file2", RENAME_WHITEOUT | RENAME_EXCHANGE), EINVAL);
    int unlink_efault = fails(unlink((const char *)bad), EFAULT);
    int unlinkat_enotdir = fails(unlinkat(regfd, "x", 0), ENOTDIR);

    if (regfd >= 0) close(regfd);
    if (dirfd >= 0) close(dirfd);
    unlink("file");
    unlink("file2");
    unlink("newlink");
    unlink("b");
    chdir("/");
    rmdir(dir);

    printf("aterr fstatat_enotdir=%d fstatat_ebadf=%d fstatat_einval=%d statx_efault=%d "
           "statx_einval_flag=%d statx_einval_mask=%d statx_enotdir=%d symlinkat_enotdir=%d "
           "linkat_enotdir=%d linkat_einval=%d linkat_exdev=%d renameat2_einval=%d "
           "unlink_efault=%d unlinkat_enotdir=%d\n",
           fstatat_enotdir, fstatat_ebadf, fstatat_einval, statx_efault, statx_einval_flag,
           statx_einval_mask, statx_enotdir, symlinkat_enotdir, linkat_enotdir, linkat_einval,
           linkat_exdev, renameat2_einval, unlink_efault, unlinkat_enotdir);
    return 0;
}
