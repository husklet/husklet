// Container process-identity self-consistency. A guest reads its own PID through four independent
// surfaces the engine must synthesize coherently: getpid(2), the first field of /proc/self/stat, the
// "Pid:" line of /proc/self/status, and the /proc/self magic symlink target. Real runtimes (Go's
// os.Getpid vs procfs, JVM ProcessHandle, supervisors reading /proc) assume all four agree. This
// prints a NORMALIZED verdict (all-agree booleans, never the raw host-variant pid), so it is byte-
// identical on a bare Linux host and a correct engine; a divergence flags a procfs identity defect.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <fcntl.h>

static long slurp_first_long(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) return -1;
    char b[512];
    int n = (int)read(fd, b, sizeof b - 1);
    close(fd);
    if (n <= 0) return -1;
    b[n] = 0;
    return strtol(b, NULL, 10);
}

static long status_field(const char *key) {
    int fd = open("/proc/self/status", O_RDONLY);
    if (fd < 0) return -1;
    char b[4096];
    int n = (int)read(fd, b, sizeof b - 1);
    close(fd);
    if (n <= 0) return -1;
    b[n] = 0;
    char *p = strstr(b, key);
    if (!p) return -1;
    return strtol(p + strlen(key), NULL, 10);
}

int main(void) {
    long me = (long)getpid();
    long stat_pid = slurp_first_long("/proc/self/stat");
    long status_pid = status_field("Pid:");
    char link[64] = {0};
    long link_pid = -1;
    if (readlink("/proc/self", link, sizeof link - 1) > 0) link_pid = strtol(link, NULL, 10);

    printf("stat_match=%d status_match=%d link_match=%d pid_positive=%d\n",
           stat_pid == me, status_pid == me, link_pid == me, me > 0);

    long ppid_status = status_field("PPid:");
    printf("ppid_match=%d\n", ppid_status == (long)getppid());
    return 0;
}
