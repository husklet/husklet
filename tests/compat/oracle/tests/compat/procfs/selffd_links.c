// /proc/self/fd is per-process state the engine must synthesise: entries appear and disappear
// with the descriptor table, readlink resolves to the opened path, O_CLOEXEC does not change
// the link, and a closed descriptor's entry is gone.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

static int link_of(int fd, char *out, size_t n) {
    char p[64];
    snprintf(p, sizeof p, "/proc/self/fd/%d", fd);
    ssize_t r = readlink(p, out, n - 1);
    if (r < 0) return -1;
    out[r] = 0;
    return 0;
}

int main(void) {
    char buf[256];
    int fd = open("/dev/null", O_RDONLY | O_CLOEXEC);
    int l1 = link_of(fd, buf, sizeof buf);
    int isnull = (strcmp(buf, "/dev/null") == 0);
    int d = dup(fd);
    int l2 = link_of(d, buf, sizeof buf);
    int dupnull = (strcmp(buf, "/dev/null") == 0);
    close(d);
    int l3 = link_of(d, buf, sizeof buf);
    int e3 = (l3 == -1) ? errno : 0;

    int pfd[2];
    if (pipe(pfd)) return 1;
    int l4 = link_of(pfd[0], buf, sizeof buf);
    int ispipe = (strncmp(buf, "pipe:[", 6) == 0);
    // opening the /proc/self/fd link of a regular file gives a working descriptor
    int tmp = open("/tmp", O_RDONLY);
    int l5 = link_of(tmp, buf, sizeof buf);
    int istmp = (strcmp(buf, "/tmp") == 0);
    // /proc/self/fd/<fd> for the directory fd itself
    char self[64];
    snprintf(self, sizeof self, "/proc/self/fd/%d", tmp);
    int reopen = open(self, O_RDONLY);
    int ok = (reopen >= 0);
    // a nonexistent descriptor number
    int l6 = link_of(9999, buf, sizeof buf);
    int e6 = (l6 == -1) ? errno : 0;
    printf("l1=%d isnull=%d l2=%d dupnull=%d l3=%d e3=%d l4=%d ispipe=%d l5=%d istmp=%d ok=%d l6=%d e6=%d\n",
           l1, isnull, l2, dupnull, l3, e3, l4, ispipe, l5, istmp, ok, l6, e6);
    return 0;
}
