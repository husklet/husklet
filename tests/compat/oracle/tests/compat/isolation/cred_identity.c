// Container credential self-consistency. The uid/gid a guest observes via syscalls must match what
// the engine renders in /proc/self/status (setuid programs, package managers, and Go's os/user read
// both). We compare getresuid/getresgid against the "Uid:" / "Gid:" lines of /proc/self/status and
// emit a normalized all-agree verdict (never the raw host-variant ids), so it is identical on a bare
// host and a correct engine; a mismatch flags a credential-synthesis defect.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/types.h>

static int status_triple(const char *key, long *a, long *b, long *c) {
    int fd = open("/proc/self/status", O_RDONLY);
    if (fd < 0) return -1;
    char buf[4096];
    int n = (int)read(fd, buf, sizeof buf - 1);
    close(fd);
    if (n <= 0) return -1;
    buf[n] = 0;
    char *p = strstr(buf, key);
    if (!p) return -1;
    p += strlen(key);
    *a = strtol(p, &p, 10);
    *b = strtol(p, &p, 10);
    *c = strtol(p, &p, 10);
    return 0;
}

int main(void) {
    uid_t ru, eu, su;
    gid_t rg, eg, sg;
    getresuid(&ru, &eu, &su);
    getresgid(&rg, &eg, &sg);

    long su_r = -1, su_e = -1, su_s = -1, sg_r = -1, sg_e = -1, sg_s = -1;
    status_triple("Uid:", &su_r, &su_e, &su_s);
    status_triple("Gid:", &sg_r, &sg_e, &sg_s);

    printf("uid_match=%d gid_match=%d\n",
           (long)ru == su_r && (long)eu == su_e && (long)su == su_s,
           (long)rg == sg_r && (long)eg == sg_e && (long)sg == sg_s);
    printf("uid_effective_real_match=%d gid_effective_real_match=%d\n",
           ru == eu, rg == eg);

    /* getuid/geteuid must agree with the resuid pair */
    printf("legacy_uid_match=%d legacy_gid_match=%d\n",
           getuid() == ru && geteuid() == eu, getgid() == rg && getegid() == eg);
    return 0;
}
