// /proc/self/fd/<n> readlink target virtualization. A fd on a regular file under a bound volume (/tmp)
// must readlink to the file's GUEST-absolute path, never the underlying host path (the engine used to leak
// the macOS /private/tmp path on x86_64 for a mapped volume). A pipe fd still readlinks to a "pipe:[...]"
// anon-inode name. Both derived from fds this process opens itself; oracle-neutral.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int link_of(int fd, char *out, int cap) {
    char p[64];
    snprintf(p, sizeof p, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(p, out, cap - 1);
    if (n <= 0) return 0;
    out[n] = 0;
    return 1;
}

int main(void) {
    char path[] = "/tmp/selffdlink_probe.XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) { printf("selffdlink ok=0\n"); return 0; }
    char link[4096];
    // The link must be the guest path (/tmp/...), not a host path (/private/tmp/... or a host scratch dir).
    int file_link_ok = link_of(fd, link, sizeof link) && strcmp(link, path) == 0;
    close(fd);
    unlink(path);

    int pf[2];
    int pipe_link_ok = 0;
    if (pipe(pf) == 0) {
        char pl[64];
        pipe_link_ok = link_of(pf[0], pl, sizeof pl) && strncmp(pl, "pipe:[", 6) == 0;
        close(pf[0]);
        close(pf[1]);
    }
    int ok = file_link_ok && pipe_link_ok;
    printf("selffdlink ok=%d\n", ok);
    return 0;
}
