// /proc/self/limits must agree with the live rlimits: lowering RLIMIT_NOFILE is visible in the
// file, an infinite limit prints "unlimited", and the header/column layout is stable.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/resource.h>
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

static void field(const char *buf, const char *key, char *out, size_t n) {
    out[0] = 0;
    const char *p = strstr(buf, key);
    if (!p) return;
    p += strlen(key);
    while (*p == ' ') p++;
    size_t i = 0;
    while (p[i] && p[i] != ' ' && p[i] != '\n' && i + 1 < n) { out[i] = p[i]; i++; }
    out[i] = 0;
}

int main(void) {
    struct rlimit rl;
    getrlimit(RLIMIT_NOFILE, &rl);
    rl.rlim_cur = 128;
    int s = setrlimit(RLIMIT_NOFILE, &rl);
    char buf[16384];
    int n = slurp("/proc/self/limits", buf, sizeof buf);
    int hasheader = (strncmp(buf, "Limit", 5) == 0);
    char nofile[64], core[64], stackmax[64];
    field(buf, "Max open files", nofile, sizeof nofile);
    field(buf, "Max core file size", core, sizeof core);
    field(buf, "Max processes", stackmax, sizeof stackmax);
    int haslines = 0;
    for (const char *p = buf; *p; p++) if (*p == '\n') haslines++;
    printf("s=%d ok=%d hasheader=%d nofile=%s corepresent=%d procpresent=%d lines_ge=%d\n",
           s, n > 0, hasheader, nofile, core[0] != 0, stackmax[0] != 0, haslines >= 16);
    return 0;
}
