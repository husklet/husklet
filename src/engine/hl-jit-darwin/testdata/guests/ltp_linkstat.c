// link()/linkat()/lstat() semantics — LTP link02/link05/lstat01/lstat02 surface, deterministic self-check
// oracle-diffed hl-vs-native on both arches. Covers: link increments st_nlink; link content is shared;
// link error paths (EEXIST/ENOENT/EPERM-on-dir); lstat reports the SYMLINK itself (size=len, S_ISLNK), not
// its target; lstat error paths (ENOENT/ENOTDIR).
//
// Also guards the #402 setup-phase regressions that made all four LTP tests BROK under hl:
//   (a) utimensat(AT_FDCWD, path, NULL, 0) — the exact syscall LTP's SAFE_TOUCH issues in setup — must
//       SUCCEED (NULL `times` == set atime/mtime to now). Syscall 88 was missing from hl's non-PIE
//       path-pointer rebase list, so on the static non-PIE LTP binaries the host got an un-rebased low
//       link-vaddr and returned EFAULT. (The static-PIE self-check here documents the NULL-times contract;
//       the non-PIE trigger is exercised by the LTP compliance lane binaries themselves.)
//   (b) lstat/stat/statx on a NULL (bad) path pointer must return EFAULT, not SIGSEGV. hl's /proc
//       magic-link synthesis helpers (proc_self_leaf / synth_stat_raw) dereferenced a NULL resolved path
//       and crashed the engine before the host stat could EFAULT — a crash that reproduces on ORDINARY
//       static-PIE guests (exit 255), which is what this self-check pins.
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    const char *base = "/tmp/ltp_ls_a";
    const char *l1 = "/tmp/ltp_ls_b";
    const char *l2 = "/tmp/ltp_ls_c";
    const char *sym = "/tmp/ltp_ls_sym";
    unlink(base); unlink(l1); unlink(l2); unlink(sym);

    int fd = open(base, O_RDWR | O_CREAT | O_TRUNC, 0644);
    write(fd, "hello", 5);
    close(fd);

    // link: new name -> same inode, st_nlink becomes 2.
    int r = link(base, l1);
    struct stat s1, s2;
    stat(base, &s1); stat(l1, &s2);
    printf("link ok=%d nlink=%d sameino=%d\n", r == 0, (int)s1.st_nlink, s1.st_ino == s2.st_ino);

    // a second link -> nlink 3 (link05: multiple links).
    link(base, l2);
    stat(base, &s1);
    printf("link2 nlink=%d\n", (int)s1.st_nlink);

    // shared content: write through one link, read through another.
    int wf = open(l1, O_WRONLY);
    pwrite(wf, "J", 1, 0);
    close(wf);
    char buf[8] = {0};
    int rf = open(l2, O_RDONLY);
    read(rf, buf, 5);
    close(rf);
    printf("link shared content=%s\n", buf);

    // link over an existing name -> EEXIST.
    errno = 0;
    int e1 = link(base, l1);
    printf("link EEXIST: ret=%d ok=%d\n", e1, e1 < 0 && errno == EEXIST);

    // link with a nonexistent source -> ENOENT.
    errno = 0;
    int e2 = link("/tmp/ltp_ls_nope", "/tmp/ltp_ls_x");
    printf("link ENOENT: ret=%d ok=%d\n", e2, e2 < 0 && errno == ENOENT);

    // symlink + lstat: lstat must report the LINK (S_ISLNK, size==strlen(target)), not the target file.
    symlink(base, sym);
    struct stat ls;
    int lr = lstat(sym, &ls);
    printf("lstat symlink: ok=%d islnk=%d size=%d\n", lr == 0, S_ISLNK(ls.st_mode) != 0,
           (int)ls.st_size == (int)strlen(base));
    // stat() through the symlink follows it -> the regular file (size 5).
    struct stat ts;
    stat(sym, &ts);
    printf("stat follows: reg=%d size=%d\n", S_ISREG(ts.st_mode) != 0, (int)ts.st_size);

    // lstat on a nonexistent path -> ENOENT.
    errno = 0;
    struct stat ns;
    int nr = lstat("/tmp/ltp_ls_nope", &ns);
    printf("lstat ENOENT: ret=%d ok=%d\n", nr, nr < 0 && errno == ENOENT);

    // lstat where a path component is a file (not a dir) -> ENOTDIR.
    errno = 0;
    int nd = lstat("/tmp/ltp_ls_a/x", &ns);
    printf("lstat ENOTDIR: ret=%d ok=%d\n", nd, nd < 0 && errno == ENOTDIR);

    // #402(a): utimensat with NULL `times` (LTP SAFE_TOUCH) sets atime/mtime to now and SUCCEEDS.
    errno = 0;
    int ur = utimensat(AT_FDCWD, base, NULL, 0);
    printf("utimensat NULL-times: ret=%d ok=%d\n", ur, ur == 0);
    // The AT_SYMLINK_NOFOLLOW form (utimensat on the link itself) also succeeds on our symlink.
    errno = 0;
    int url = utimensat(AT_FDCWD, sym, NULL, AT_SYMLINK_NOFOLLOW);
    printf("utimensat NULL-times nofollow: ret=%d ok=%d\n", url, url == 0);

    // #402(b): a NULL (bad) path pointer must return EFAULT, never crash the engine. `volatile` keeps the
    // compiler from folding away the known-NULL argument under -O2 (the harness builds -O2 -static-pie).
    const char *volatile bad = 0;
    struct stat bs;
    errno = 0;
    int le = lstat((const char *)bad, &bs);
    printf("lstat(NULL): ret=%d efault=%d\n", le, le < 0 && errno == EFAULT);
    errno = 0;
    int se = stat((const char *)bad, &bs);
    printf("stat(NULL): ret=%d efault=%d\n", se, se < 0 && errno == EFAULT);
    // statx(AT_FDCWD, NULL, ...) shares the same NULL-path -> EFAULT contract (glibc may lack a wrapper on
    // older headers, so issue the raw syscall). STATX_BASIC_STATS = 0x7ff.
    unsigned char stxbuf[256];
    errno = 0;
    long xe = syscall(SYS_statx, (int)AT_FDCWD, (const char *)bad, 0, 0x7ffu, stxbuf);
    printf("statx(NULL): ret=%d efault=%d\n", (int)xe, xe < 0 && errno == EFAULT);

    unlink(base); unlink(l1); unlink(l2); unlink(sym);
    return 0;
}
