// Container rlimit coherence between the syscall surface and procfs. getrlimit(2) and the
// /proc/self/limits table must agree (glibc, the JVM, and systemd read both). We check NOFILE, NPROC,
// STACK, and AS for the cur<=max invariant and cross-check NOFILE's soft/hard against the parsed
// /proc/self/limits row. Output is a normalized verdict (agreement + invariant booleans, never the
// host-variant numeric caps), identical on a bare host and a correct engine.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <stdlib.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/resource.h>

static int sane(int res) {
    struct rlimit r;
    if (getrlimit(res, &r) != 0) return -1;
    if (r.rlim_max == RLIM_INFINITY) return 1;
    if (r.rlim_cur == RLIM_INFINITY) return 0; /* cur infinite but max finite -> invalid */
    return r.rlim_cur <= r.rlim_max;
}

/* parse the "Max open files" row of /proc/self/limits: soft then hard */
static int limits_nofile(unsigned long long *soft, unsigned long long *hard) {
    int fd = open("/proc/self/limits", O_RDONLY);
    if (fd < 0) return -1;
    char b[8192];
    int n = (int)read(fd, b, sizeof b - 1);
    close(fd);
    if (n <= 0) return -1;
    b[n] = 0;
    char *p = strstr(b, "Max open files");
    if (!p) return -1;
    p += strlen("Max open files");
    char *end;
    *soft = strtoull(p, &end, 10);
    *hard = strtoull(end, &end, 10);
    return 0;
}

int main(void) {
    printf("nofile_sane=%d nproc_sane=%d stack_sane=%d as_sane=%d\n",
           sane(RLIMIT_NOFILE), sane(RLIMIT_NPROC), sane(RLIMIT_STACK), sane(RLIMIT_AS));

    struct rlimit r;
    getrlimit(RLIMIT_NOFILE, &r);
    unsigned long long ls = 0, lh = 0;
    int have = limits_nofile(&ls, &lh) == 0;
    int soft_match = have && ((r.rlim_cur == RLIM_INFINITY) ? 0 : (r.rlim_cur == ls));
    int hard_match = have && ((r.rlim_max == RLIM_INFINITY) ? 0 : (r.rlim_max == lh));
    printf("limits_present=%d nofile_soft_match=%d nofile_hard_match=%d\n",
           have, soft_match, hard_match);
    return 0;
}
