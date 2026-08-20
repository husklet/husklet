// The container DAC is VIRTUAL (linux_abi/container/dac_policy.h): guest ids, guest modes and the guest
// capability set decide, and the host tree is storage owned by the engine's own unprivileged uid. So guest
// root, holding CAP_DAC_OVERRIDE, must be able to read and write its own `chmod 000` file -- the host
// open() of that file denies on the host mode bits, and that denial is not a boundary: the engine owns the
// inode and could chmod it at will.
//
// Every field is a behavior the fix must produce, plus the guards that keep the case honest:
//   * `mode` re-reads the guest-visible mode AFTER the privileged access, so lending the host bits
//     permanently (instead of restoring them) fails here rather than passing;
//   * `dropped` and `child_open_ok` prove the unprivileged child really dropped and can still open a
//     readable file -- without them `child_denied` would be satisfied by a child that failed at everything;
//   * `child_denied` is the non-vacuity guard on the fix itself: the virtual DAC must still be the layer
//     that decides, so uid 2000 must NOT get the same lend guest root gets.
// `slash` covers the second defect: Linux ignores trailing slashes on a create target, and splitting the
// parent at the trailing '/' made the parent the not-yet-existing target -> ENOENT on every `mkdir foo/`,
// which is how `git clone` failed on ".git/branches/".
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <sys/xattr.h>
#include <unistd.h>

#define DROP_UID 2000

static int read_first(const char *path, char *out, size_t size) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t got = read(fd, out, size - 1);
    close(fd);
    if (got < 0) return -1;
    out[got] = 0;
    return 0;
}

int main(void) {
    if (mkdir("/tmp/dachost", 0755) != 0 && errno != EEXIST) return 1;
    // Create the file ALREADY closed rather than chmod-ing it afterwards: a guest chmod is recorded in the
    // engine's own virtual-mode overlay and leaves the host inode permissive, so a post-hoc chmod never
    // reaches the host bits and the case would pass against no fix at all. The creation mode does reach
    // them, which is the state an image file with restrictive modes is in.
    int fd = open("/tmp/dachost/closed", O_CREAT | O_WRONLY | O_TRUNC, 0);
    if (fd < 0 || write(fd, "secret\n", 7) != 7) return 1;
    close(fd);
    fd = open("/tmp/dachost/readonly", O_CREAT | O_WRONLY | O_TRUNC, 0444);
    if (fd < 0 || write(fd, "frozen\n", 7) != 7) return 1;
    close(fd);
    fd = open("/tmp/dachost/readable", O_CREAT | O_RDWR | O_TRUNC, 0644);
    if (fd < 0 || write(fd, "public\n", 7) != 7) return 1;
    close(fd);

    char buffer[64];
    unsigned root_read = read_first("/tmp/dachost/closed", buffer, sizeof buffer) == 0 && !strcmp(buffer, "secret\n");
    // Root rewriting a 0444 file: the host owner bits carry no write bit, so the host open denies where a
    // native root, holding CAP_DAC_OVERRIDE, succeeds.
    fd = open("/tmp/dachost/readonly", O_WRONLY);
    unsigned root_write = fd >= 0 && write(fd, "S", 1) == 1;
    if (fd >= 0) close(fd);
    // getxattr needs host read permission too; an authorized guest root must see "no such attribute",
    // never a permission error.
    errno = 0;
    unsigned xattr_nodata = getxattr("/tmp/dachost/closed", "user.absent", buffer, sizeof buffer) < 0 &&
                            errno == ENODATA;
    struct stat status;
    unsigned mode = lstat("/tmp/dachost/closed", &status) == 0 ? (unsigned)(status.st_mode & 07777) : 0xffffu;
    unsigned readonly_mode =
        lstat("/tmp/dachost/readonly", &status) == 0 ? (unsigned)(status.st_mode & 07777) : 0xffffu;

    rmdir("/tmp/dachost/slash");
    unsigned slash = mkdir("/tmp/dachost/slash/", 0755) == 0 && lstat("/tmp/dachost/slash", &status) == 0 &&
                     S_ISDIR(status.st_mode);

    int pipes[2];
    if (pipe(pipes) != 0) return 1;
    pid_t child = fork();
    if (child == 0) {
        close(pipes[0]);
        unsigned report[3] = {0, 0, 0};
        if (setresgid(DROP_UID, DROP_UID, DROP_UID) == 0 && setresuid(DROP_UID, DROP_UID, DROP_UID) == 0)
            report[0] = getuid() == DROP_UID;
        report[1] = read_first("/tmp/dachost/closed", buffer, sizeof buffer) != 0 && errno == EACCES;
        report[2] = read_first("/tmp/dachost/readable", buffer, sizeof buffer) == 0;
        ssize_t sent = write(pipes[1], report, sizeof report);
        close(pipes[1]);
        _exit(sent == (ssize_t)sizeof report ? 0 : 1);
    }
    close(pipes[1]);
    unsigned report[3] = {0, 0, 0};
    ssize_t got = read(pipes[0], report, sizeof report);
    close(pipes[0]);
    int wait_status = 0;
    waitpid(child, &wait_status, 0);
    if (got != (ssize_t)sizeof report) return 1;

    printf("dac-host-mode root-read=%u root-write=%u xattr-nodata=%u mode=%04o readonly=%04o slash=%u "
           "dropped=%u child-denied=%u child-open=%u\n",
           root_read, root_write, xattr_nodata, mode, readonly_mode, slash, report[0], report[1], report[2]);
    return 0;
}
