#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static int write_file(const char *path) {
    int fd = open(path, O_CREAT | O_TRUNC | O_WRONLY, 0644);
    if (fd < 0) return -1;
    int ok = write(fd, "ok", 2) == 2 && close(fd) == 0;
    return ok ? 0 : -1;
}

int main(void) {
    char base[128], run[160], file[192], absolute[192], relative[192], chain[192], loop_a[192], loop_b[192];
    snprintf(base, sizeof base, "/tmp/hl-absolute-link-%d", getpid());
    snprintf(run, sizeof run, "%s/run", base);
    snprintf(file, sizeof file, "%s/value", run);
    snprintf(absolute, sizeof absolute, "%s/absolute", base);
    snprintf(relative, sizeof relative, "%s/relative", base);
    snprintf(chain, sizeof chain, "%s/chain", base);
    snprintf(loop_a, sizeof loop_a, "%s/loop-a", base);
    snprintf(loop_b, sizeof loop_b, "%s/loop-b", base);
    if (mkdir(base, 0755) || mkdir(run, 0755) || write_file(file)) return 1;
    if (symlink(run, absolute) || symlink("run", relative) || symlink(absolute, chain)) return 2;
    if (symlink("loop-b", loop_a) || symlink("loop-a", loop_b)) return 3;

    char path[224], value[8] = {0}, target[256];
    snprintf(path, sizeof path, "%s/value", absolute);
    int fd = open(path, O_RDONLY);
    int absolute_ok = fd >= 0 && read(fd, value, 2) == 2 && !memcmp(value, "ok", 2);
    if (fd >= 0) close(fd);
    memset(value, 0, sizeof value);
    snprintf(path, sizeof path, "%s/../relative/value", absolute);
    fd = open(path, O_RDONLY);
    int dotdot_ok = fd >= 0 && read(fd, value, 2) == 2 && !memcmp(value, "ok", 2);
    if (fd >= 0) close(fd);
    snprintf(path, sizeof path, "%s/value", chain);
    int chain_ok = access(path, R_OK) == 0;
    ssize_t length = readlink(absolute, target, sizeof target);
    int nofollow_ok = length == (ssize_t)strlen(run) && !memcmp(target, run, (size_t)length);
    errno = 0;
    int loop_ok = open(loop_a, O_RDONLY) < 0 && errno == ELOOP;

    unlink(loop_b); unlink(loop_a); unlink(chain); unlink(relative); unlink(absolute); unlink(file); rmdir(run); rmdir(base);
    printf("absolute-symlink absolute=%d relative-dotdot=%d chain=%d nofollow=%d eloop=%d\n",
           absolute_ok, dotdot_ok, chain_ok, nofollow_ok, loop_ok);
    return !(absolute_ok && dotdot_ok && chain_ok && nofollow_ok && loop_ok);
}
