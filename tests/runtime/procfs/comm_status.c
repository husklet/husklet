// /proc/self/comm and /proc/self/status must track prctl(PR_SET_NAME): the name is truncated
// to 15 characters plus NUL, comm reads back with a trailing newline, status Name: agrees, and
// writing comm updates prctl's view.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <unistd.h>

static int slurp(const char *p, char *b, size_t n) {
    int fd = open(p, O_RDONLY);
    if (fd < 0) return -1;
    ssize_t r = read(fd, b, n - 1);
    close(fd);
    if (r < 0) return -1;
    b[r] = 0;
    return (int)r;
}

static void status_name(char *out, size_t n) {
    char buf[8192];
    out[0] = 0;
    if (slurp("/proc/self/status", buf, sizeof buf) < 0) return;
    char *p = strstr(buf, "Name:");
    if (!p) return;
    p += 5;
    while (*p == ' ' || *p == '\t')
        p++;
    size_t i = 0;
    while (p[i] && p[i] != '\n' && i + 1 < n) {
        out[i] = p[i];
        i++;
    }
    out[i] = 0;
}

int main(void) {
    int s = prctl(PR_SET_NAME, "abcdefghijklmnopqrstuvwxyz", 0, 0, 0);
    char got[64] = {0};
    int g = prctl(PR_GET_NAME, got, 0, 0, 0);
    char comm[64];
    int n = slurp("/proc/self/comm", comm, sizeof comm);
    int nl = (n > 0 && comm[n - 1] == '\n');
    if (nl) comm[n - 1] = 0;
    char sname[64];
    status_name(sname, sizeof sname);

    int fd = open("/proc/self/comm", O_RDWR);
    int peer = fd >= 0 ? dup(fd) : -1;
    ssize_t first = (fd >= 0) ? write(fd, "long-written-name", 17) : -1;
    ssize_t w = first == 17 && lseek(fd, 0, SEEK_SET) == 0 ? write(fd, "written-name", 12) : first;
    int same_fd = 0;
    if (w == 12 && peer >= 0 && lseek(fd, 0, SEEK_SET) == 0) {
        char same[64];
        ssize_t r = read(peer, same, sizeof same - 1);
        if (r >= 0) {
            same[r] = 0;
            same_fd = strcmp(same, "written-name\n") == 0;
        }
    }
    if (peer >= 0) close(peer);
    if (fd >= 0) close(fd);
    char got2[64] = {0};
    prctl(PR_GET_NAME, got2, 0, 0, 0);
    if (!same_fd) got2[0] = 0;

    // A newline is an ordinary character in a comm write: Linux terminates the name at a NUL only,
    // so "ab\ncd" reads back verbatim. A zero-length write clears the name rather than being ignored.
    char probe[64];
    int fd2 = open("/proc/self/comm", O_WRONLY);
    ssize_t nlw = fd2 >= 0 ? write(fd2, "ab\ncd", 5) : -1;
    if (fd2 >= 0) close(fd2);
    int n2 = slurp("/proc/self/comm", probe, sizeof probe);
    int nl_kept = nlw == 5 && n2 == 6 && memcmp(probe, "ab\ncd\n", 6) == 0;

    fd2 = open("/proc/self/comm", O_WRONLY);
    ssize_t zw = fd2 >= 0 ? write(fd2, "", 0) : -1;
    if (fd2 >= 0) close(fd2);
    n2 = slurp("/proc/self/comm", probe, sizeof probe);
    int cleared = zw == 0 && n2 == 1 && probe[0] == '\n';

    printf("s=%d g=%d name=%s len=%zu comm=%s nl=%d status=%s w=%zd after=%s nl_kept=%d cleared=%d\n", s, g, got,
           strlen(got), comm, nl, sname, w, got2, nl_kept, cleared);
    return 0;
}
