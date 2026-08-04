// /proc/self/cgroup is what systemd, the JVM's container-awareness and cgroup tooling read to locate their
// control group. On a cgroup v2 (unified) container it must be a single line of the exact form "0::/<path>"
// — hierarchy id 0, empty controller field, an absolute cgroup path. Assert that structural shape (the
// three colon-separated fields with the fixed "0" and "" and a leading '/'), not the host-variant path
// text. A v1-style multi-line or malformed cgroup file breaks container detection. Derived structural.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include "pf.h"

int main(void) {
    char b[4096];
    int n = pf_read("/proc/self/cgroup", b, sizeof b);
    // first (and on v2 only) line: "0::/..."
    char *nl = strchr(b, '\n');
    if (nl) *nl = 0;
    // split on ':' into exactly three fields (path may itself be empty-root "/").
    char *c1 = strchr(b, ':');
    char *c2 = c1 ? strchr(c1 + 1, ':') : NULL;
    int shape_ok = 0;
    if (c1 && c2) {
        *c1 = 0; *c2 = 0;
        const char *hier = b, *ctrl = c1 + 1, *path = c2 + 1;
        shape_ok = strcmp(hier, "0") == 0 && ctrl[0] == 0 && path[0] == '/';
    }
    struct stat st;
    int path_ok = stat("/proc/self/cgroup", &st) == 0 && S_ISREG(st.st_mode) &&
                  faccessat(AT_FDCWD, "/proc/self/cgroup", F_OK, 0) == 0;
    int ok = n > 0 && shape_ok && path_ok;
    printf("selfcgroup ok=%d\n", ok);
    return 0;
}
