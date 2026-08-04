// /proc/self/cmdline is the NUL-separated argv the process was launched with. ps, top and every
// language runtime that reports its own command line parse it by splitting on NUL. The invariants a
// correct kernel guarantees, independent of the actual argv strings: the reconstructed token count
// equals argc, token 0 equals argv[0] byte-for-byte, and the buffer is NUL-terminated (not space
// separated, not truncated). Derived from this process's own argv, so oracle-neutral.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char b[4096];
    int fd = open("/proc/self/cmdline", O_RDONLY);
    int len = 0, r;
    while (fd >= 0 && len < (int)sizeof b && (r = (int)read(fd, b + len, sizeof b - len)) > 0) len += r;
    if (fd >= 0) close(fd);
    int nul_term = len > 0 && b[len - 1] == 0;
    int count = 0;
    for (int i = 0; i < len; i++) if (b[i] == 0) count++;
    int argv0_ok = len > 0 && strcmp(b, argv[0]) == 0;
    int ok = nul_term && count == argc && argv0_ok;
    printf("selfcmdline ok=%d\n", ok);
    return 0;
}
