// Container cgroup-v2 self-membership format. /proc/self/cgroup on a unified (v2) host is a single
// line of the exact form "0::<path>" with an absolute path — systemd, runc, and the JVM's
// CgroupSubsystemFactory parse precisely this. We emit a normalized verdict (well-formedness booleans,
// never the host-variant path) so it is byte-identical on a bare v2 host and a correct engine; a
// malformed synthesis (wrong hierarchy id, missing "::", relative path) flips the verdict.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>

int main(void) {
    int fd = open("/proc/self/cgroup", O_RDONLY);
    if (fd < 0) { printf("open_ok=0\n"); return 0; }
    char b[4096];
    int n = (int)read(fd, b, sizeof b - 1);
    close(fd);
    if (n <= 0) { printf("read_ok=0\n"); return 0; }
    b[n] = 0;
    /* first line */
    char *nl = strchr(b, '\n');
    if (nl) *nl = 0;

    int v2_prefix = strncmp(b, "0::", 3) == 0;
    const char *path = v2_prefix ? b + 3 : "";
    int abs_path = path[0] == '/';
    int single_line = (nl == NULL) || (nl[1] == 0);

    printf("open_ok=1 v2_prefix=%d abs_path=%d single_line=%d\n",
           v2_prefix, abs_path, single_line);
    return 0;
}
