// dup/dup2/dup3 share one open file description: offset and status flags are shared,
// but the FD_CLOEXEC descriptor flag is per-descriptor. dup3 requires distinct fds.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(void) {
    char dir[128];
    snprintf(dir, sizeof dir, "/tmp/hl_dup_%d", (int)getpid());
    mkdir(dir, 0755);
    char path[192];
    snprintf(path, sizeof path, "%s/file", dir);
    int fd = open(path, O_CREAT | O_RDWR | O_TRUNC, 0644);
    write(fd, "0123456789", 10);
    lseek(fd, 3, SEEK_SET);

    // dup shares the offset: reading from the dup advances the original.
    int d = dup(fd);
    char c;
    read(d, &c, 1);
    int shared_offset = lseek(fd, 0, SEEK_CUR) == 4 && c == '3';

    // dup shares status flags: setting O_APPEND via one is visible via the other.
    fcntl(d, F_SETFL, O_APPEND);
    int shared_flags = (fcntl(fd, F_GETFL) & O_APPEND) != 0;

    // FD_CLOEXEC is per-descriptor: dup clears it on the new fd.
    fcntl(fd, F_SETFD, FD_CLOEXEC);
    int d2 = dup(fd);
    int cloexec_orig = (fcntl(fd, F_GETFD) & FD_CLOEXEC) != 0;
    int cloexec_dup = (fcntl(d2, F_GETFD) & FD_CLOEXEC) == 0;

    // dup2 to an explicit target number returns that number.
    int target = 20;
    int r = dup2(fd, target);
    int dup2_ok = r == target;

    // dup3 with equal fds is EINVAL; with O_CLOEXEC it sets the flag.
    errno = 0;
    int same = dup3(fd, fd, O_CLOEXEC);
    int dup3_einval = same == -1 && errno == EINVAL;
    int d3 = dup3(fd, 21, O_CLOEXEC);
    int dup3_cloexec = d3 == 21 && (fcntl(21, F_GETFD) & FD_CLOEXEC) != 0;

    close(fd); close(d); close(d2); close(target); close(21);
    unlink(path);
    rmdir(dir);
    printf("dup-offset shared-offset=%d shared-flags=%d cloexec-orig=%d cloexec-dup=%d dup2=%d dup3-einval=%d dup3-cloexec=%d\n",
           shared_offset, shared_flags, cloexec_orig, cloexec_dup, dup2_ok, dup3_einval, dup3_cloexec);
    return 0;
}
