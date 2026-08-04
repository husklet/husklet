// /proc/self/fd/<n> is the per-descriptor magic-link directory lsof, the JVM and libuv scan to learn what
// each fd points at. Kernel guarantees exercised here, all derived from fds this process opens itself:
//   * a fd on a regular file readlinks to that file's absolute path;
//   * opening the file adds exactly its entry to /proc/self/fd, closing it removes the entry;
//   * a pipe fd readlinks to a "pipe:[...]" anon-inode name (not a path).
// A synthesized fd dir that lags the live table (stale entry after close, missing entry after open, or a
// wrong link target) fails. Oracle-neutral.
#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <unistd.h>

static int fd_count(void) {
    struct rlimit limit;
    if (getrlimit(RLIMIT_NOFILE, &limit) != 0) return -1;
    DIR *d = opendir("/proc/self/fd");
    if (!d) return -1;
    int c = 0;
    for (struct dirent *e; (e = readdir(d));) {
        if (e->d_name[0] == '.') continue;
        char *end = NULL;
        unsigned long descriptor = strtoul(e->d_name, &end, 10);
        if (!end || *end || descriptor >= limit.rlim_cur) {
            closedir(d);
            return -1;
        }
        c++;
    }
    closedir(d);
    return c;
}

static int link_of(int fd, char *out, int cap) {
    char p[64];
    snprintf(p, sizeof p, "/proc/self/fd/%d", fd);
    ssize_t n = readlink(p, out, cap - 1);
    if (n <= 0) return 0;
    out[n] = 0;
    return 1;
}

int main(void) {
    char path[] = "/tmp/selffd_probe.XXXXXX";
    int fd = mkstemp(path);
    if (fd < 0) {
        printf("selffd ok=0\n");
        return 0;
    }
    int before = fd_count();
    char link[4096];
    int file_link_ok = link_of(fd, link, sizeof link) && strcmp(link, path) == 0;
    // closing removes the entry; count drops by one.
    close(fd);
    int after_close = fd_count();
    int close_removes = after_close == before - 1;
    // a pipe reads back as an anon "pipe:[...]" inode name.
    int pf[2];
    int pipe_link_ok = 0;
    if (pipe(pf) == 0) {
        char pl[64];
        pipe_link_ok = link_of(pf[0], pl, sizeof pl) && strncmp(pl, "pipe:[", 6) == 0;
        close(pf[0]);
        close(pf[1]);
    }
    unlink(path);
    int ok = before > 0 && file_link_ok && close_removes && pipe_link_ok;
    printf("selffd ok=%d\n", ok);
    return 0;
}
