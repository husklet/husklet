// O_NOFOLLOW and AT_SYMLINK_NOFOLLOW final-component semantics on an ordinary (non-jailed) path.
//   * open(O_NOFOLLOW) on a final symlink -> ELOOP; on a non-symlink -> opens normally.
//   * O_NOFOLLOW rejects only the FINAL component -- a symlink in the MIDDLE of the path still resolves.
//   * O_PATH|O_NOFOLLOW on a symlink opens the LINK ITSELF (readlinkat(fd,"") returns its target).
//   * faccessat2(AT_SYMLINK_NOFOLLOW) tests the LINK NODE: a dangling link exists; the target does not.
// hl folded a plain O_NOFOLLOW into a follow-open (opening the target, no ELOOP) and ignored
// AT_SYMLINK_NOFOLLOW in faccessat (following to the missing target -> spurious ENOENT).
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef AT_SYMLINK_NOFOLLOW
#define AT_SYMLINK_NOFOLLOW 0x100
#endif

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_symnf_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[256];

    // dir/real (a regular file), dir/link -> real, dir/sub/ (a real dir), dir/dlink -> sub (a dir symlink)
    snprintf(path, sizeof path, "%s/real", dir);
    int fd = open(path, O_CREAT | O_WRONLY, 0644);
    if (fd >= 0) {
        write(fd, "hi", 2);
        close(fd);
    }
    snprintf(path, sizeof path, "%s/sub", dir);
    mkdir(path, 0755);
    snprintf(path, sizeof path, "%s/sub/leaf", dir);
    fd = open(path, O_CREAT | O_WRONLY, 0644);
    if (fd >= 0) close(fd);
    char t[256];
    snprintf(t, sizeof t, "%s/real", dir);
    snprintf(path, sizeof path, "%s/link", dir);
    symlink(t, path);
    snprintf(path, sizeof path, "%s/dlink", dir);
    symlink("sub", path); // relative dir symlink resolved against dir
    snprintf(path, sizeof path, "%s/dangling", dir);
    symlink("no-such-target", path);

    // O_NOFOLLOW on the final symlink -> ELOOP.
    snprintf(path, sizeof path, "%s/link", dir);
    errno = 0;
    int nofollow_eloop = open(path, O_RDONLY | O_NOFOLLOW) < 0 && errno == ELOOP;

    // O_NOFOLLOW on a non-symlink final component -> opens fine.
    snprintf(path, sizeof path, "%s/real", dir);
    int nofollow_reg = open(path, O_RDONLY | O_NOFOLLOW);
    int nofollow_reg_ok = nofollow_reg >= 0;
    if (nofollow_reg >= 0) close(nofollow_reg);

    // O_NOFOLLOW rejects only the FINAL component: a symlink in the MIDDLE still resolves.
    snprintf(path, sizeof path, "%s/dlink/leaf", dir);
    int mid = open(path, O_RDONLY | O_NOFOLLOW);
    int mid_follows = mid >= 0;
    if (mid >= 0) close(mid);

    // O_PATH|O_NOFOLLOW on a symlink opens the LINK; readlinkat(fd,"") returns its target text.
    snprintf(path, sizeof path, "%s/link", dir);
    int lfd = open(path, O_PATH | O_NOFOLLOW);
    char rl[256];
    struct stat link_status;
    int opath_link_type = lfd >= 0 && fstat(lfd, &link_status) == 0 && S_ISLNK(link_status.st_mode);
    ssize_t n = lfd >= 0 ? readlinkat(lfd, "", rl, sizeof rl) : -1;
    int opath_link_target = n == (ssize_t)strlen(t) && !memcmp(rl, t, (size_t)n);
    errno = 0;
    int opath_link_io = lfd >= 0 && read(lfd, rl, sizeof rl) < 0 && errno == EBADF;
    if (lfd >= 0) close(lfd);

    // The same node semantics apply when the target does not exist: opening and inspecting the link itself
    // succeeds, while following it still fails below.
    snprintf(path, sizeof path, "%s/dangling", dir);
    int dangling_fd = open(path, O_PATH | O_NOFOLLOW);
    struct stat dangling_status;
    int dangling_opath_type =
        dangling_fd >= 0 && fstat(dangling_fd, &dangling_status) == 0 && S_ISLNK(dangling_status.st_mode);
    n = dangling_fd >= 0 ? readlinkat(dangling_fd, "", rl, sizeof rl) : -1;
    static const char dangling_target[] = "no-such-target";
    int dangling_opath_target = n == (ssize_t)(sizeof dangling_target - 1) && !memcmp(rl, dangling_target, (size_t)n);
    if (dangling_fd >= 0) close(dangling_fd);

    // faccessat2 AT_SYMLINK_NOFOLLOW: the dangling link node exists; following it is ENOENT.
    snprintf(path, sizeof path, "%s/dangling", dir);
    int dangling_nofollow = syscall(__NR_faccessat2, AT_FDCWD, path, F_OK, AT_SYMLINK_NOFOLLOW) == 0;
    errno = 0;
    int dangling_follow_enoent = faccessat(AT_FDCWD, path, F_OK, 0) != 0 && errno == ENOENT;

    snprintf(path, sizeof path, "%s/dangling", dir);
    unlink(path);
    snprintf(path, sizeof path, "%s/dlink", dir);
    unlink(path);
    snprintf(path, sizeof path, "%s/link", dir);
    unlink(path);
    snprintf(path, sizeof path, "%s/sub/leaf", dir);
    unlink(path);
    snprintf(path, sizeof path, "%s/sub", dir);
    rmdir(path);
    snprintf(path, sizeof path, "%s/real", dir);
    unlink(path);
    rmdir(dir);

    printf("symlink-nofollow eloop=%d reg=%d mid-follows=%d opath-link-type=%d opath-link-target=%d "
           "opath-link-io=%d dangling-opath-type=%d dangling-opath-target=%d dangling-node=%d "
           "dangling-follow-enoent=%d\n",
           nofollow_eloop, nofollow_reg_ok, mid_follows, opath_link_type, opath_link_target, opath_link_io,
           dangling_opath_type, dangling_opath_target, dangling_nofollow, dangling_follow_enoent);
    return 0;
}
