// chmod(2) against a path that names a directory rather than a child. `.`, `..`, and a trailing
// `/.` or `/..` all resolve to a directory root, which names no final component; Linux applies the
// mode to that directory instead of failing. The engine used to reject the whole family with EINVAL.
// Every fact is a derived boolean over an observed mode, so the golden is exact.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

// Applies `mode` through `spelling`, then reads the mode back through an independent absolute path.
static int applied(const char *directory, const char *spelling, mode_t mode) {
    if (chmod(spelling, mode) != 0) return 0;
    struct stat status;
    if (stat(directory, &status) != 0) return 0;
    return (status.st_mode & 07777) == mode;
}

int main(void) {
    char root[128], child[192];
    snprintf(root, sizeof root, "/tmp/hl_chmoddot_%d", (int)getpid());
    if (mkdir(root, 0755) != 0) return 2;
    snprintf(child, sizeof child, "%s/inner", root);
    if (mkdir(child, 0755) != 0) return 3;
    // Work from inside the child so `.` and `..` name two different directories.
    if (chdir(child) != 0) return 4;

    // Bare `.` -> the current directory.
    int dot = applied(child, ".", 0710);
    // Bare `..` -> the parent directory.
    int dotdot = applied(root, "..", 0711);
    // Trailing `/.` and `/..` on an absolute path.
    char trailing_dot[256], trailing_dotdot[256];
    snprintf(trailing_dot, sizeof trailing_dot, "%s/.", child);
    snprintf(trailing_dotdot, sizeof trailing_dotdot, "%s/..", child);
    int slash_dot = applied(child, trailing_dot, 0712);
    int slash_dotdot = applied(root, trailing_dotdot, 0713);

    // fchmodat with a dirfd and a bare `.` operand: same resolution, dirfd-relative.
    int dirfd = open(child, O_RDONLY | O_DIRECTORY);
    if (dirfd < 0) return 5;
    int at_dot = fchmodat(dirfd, ".", 0714, 0) == 0;
    struct stat after;
    at_dot = at_dot && stat(child, &after) == 0 && (after.st_mode & 07777) == 0714;

    // AT_EMPTY_PATH over the directory fd itself: the empty-operand form, which already worked.
    int at_empty = syscall(__NR_fchmodat2, dirfd, "", 0715, AT_EMPTY_PATH) == 0;
    at_empty = at_empty && stat(child, &after) == 0 && (after.st_mode & 07777) == 0715;

    // fchmod on the directory fd, the descriptor-only spelling.
    int by_fd = fchmod(dirfd, 0716) == 0 && stat(child, &after) == 0 && (after.st_mode & 07777) == 0716;

    printf("dot=%d dotdot=%d slash-dot=%d slash-dotdot=%d\n", dot, dotdot, slash_dot, slash_dotdot);
    printf("at-dot=%d at-empty=%d by-fd=%d\n", at_dot, at_empty, by_fd);

    close(dirfd);
    if (chdir("/") != 0) return 6;
    rmdir(child);
    rmdir(root);
    return 0;
}
