// Container hostname coherence. gethostname(2), uname(2).nodename, and /proc/sys/kernel/hostname are
// three surfaces onto the same UTS value; a guest (and clustered software that keys on node identity)
// expects them identical. We emit a normalized verdict — agreement booleans and a non-empty check,
// never the host-variant name — so it is byte-identical on a bare host and a correct engine.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/utsname.h>

int main(void) {
    char hn[256] = {0};
    int gh = gethostname(hn, sizeof hn - 1);

    struct utsname u;
    int un = uname(&u);

    char proc[256] = {0};
    int fd = open("/proc/sys/kernel/hostname", O_RDONLY);
    int pn = -1;
    if (fd >= 0) {
        int n = (int)read(fd, proc, sizeof proc - 1);
        close(fd);
        if (n > 0) {
            while (n > 0 && (proc[n - 1] == '\n' || proc[n - 1] == '\r')) n--;
            proc[n] = 0;
            pn = 0;
        }
    }

    int uts_match = (gh == 0 && un == 0) ? (strcmp(hn, u.nodename) == 0) : 0;
    int proc_match = (gh == 0 && pn == 0) ? (strcmp(hn, proc) == 0) : 0;

    printf("gethostname_ok=%d uname_ok=%d proc_ok=%d\n", gh == 0, un == 0, pn == 0);
    printf("uts_match=%d proc_match=%d nonempty=%d\n", uts_match, proc_match, hn[0] != 0);
    printf("sysname_is_linux=%d\n", un == 0 && strcmp(u.sysname, "Linux") == 0);
    return 0;
}
