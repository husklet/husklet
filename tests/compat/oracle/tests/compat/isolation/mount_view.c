// Container mount-table shape. A guest parsing /proc/self/mountinfo must find a root ("/") mount and
// the pseudo-filesystems software depends on: a proc mount at /proc and a sysfs mount at /sys. We
// emit a normalized verdict (presence booleans + a well-formedness check that every line carries a
// mount point), never the host-variant device/option strings, so it is identical on a bare host and a
// correct engine; a missing pseudo-fs or a structurally malformed mountinfo flips the verdict.
#define _GNU_SOURCE
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>

int main(void) {
    int fd = open("/proc/self/mountinfo", O_RDONLY);
    if (fd < 0) { printf("open_ok=0\n"); return 0; }
    static char b[65536];
    int n = (int)read(fd, b, sizeof b - 1);
    close(fd);
    if (n <= 0) { printf("read_ok=0\n"); return 0; }
    b[n] = 0;

    int has_root = 0, has_proc = 0, has_sys = 0, wellformed = 1, lines = 0;
    for (char *line = strtok(b, "\n"); line; line = strtok(NULL, "\n")) {
        lines++;
        /* mountinfo: field 5 is mount point (1-indexed): id parent major:minor root MOUNTPOINT ... */
        char *save;
        char copy[4096];
        snprintf(copy, sizeof copy, "%s", line);
        char *tok = strtok_r(copy, " ", &save);
        char *mp = NULL;
        for (int f = 1; tok; f++, tok = strtok_r(NULL, " ", &save)) {
            if (f == 5) { mp = tok; break; }
        }
        if (mp) {
            if (strcmp(mp, "/") == 0) has_root = 1;
            if (strcmp(mp, "/proc") == 0) has_proc = 1;
            if (strcmp(mp, "/sys") == 0) has_sys = 1;
            if (mp[0] != '/') wellformed = 0;   /* every mount point is absolute */
        } else {
            wellformed = 0;                      /* line lacked a 5th field */
        }
    }
    printf("open_ok=1 has_root=%d has_proc=%d has_sys=%d wellformed=%d lines_positive=%d\n",
           has_root, has_proc, has_sys, wellformed, lines > 0);
    return 0;
}
