// EDGE: /proc/self/task — the per-thread directory crashpad/ThreadHelpers (and any thread enumerator)
// walks. opendir(/proc/self/task) must list >=1 numeric tid, our own tid's task dir must stat as a
// directory, and its per-thread stat/comm must be readable & non-empty. Verdict-checked (tid values
// vary, and hl models the main thread where native Linux may show more) so it's golden across engines.
#include <dirent.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>

int main(void) {
    // count numeric tid entries under /proc/self/task
    DIR *d = opendir("/proc/self/task");
    int tids = 0;
    if (d) {
        struct dirent *e;
        while ((e = readdir(d)))
            if (e->d_name[0] >= '0' && e->d_name[0] <= '9') tids++;
        closedir(d);
    }

    // our own thread's task dir must exist as a directory
    long tid = syscall(SYS_gettid);
    char p[64];
    struct stat st;
    snprintf(p, sizeof p, "/proc/self/task/%ld", tid);
    int isdir = (stat(p, &st) == 0) && S_ISDIR(st.st_mode);

    // per-thread stat + comm must be readable and non-empty
    char buf[256];
    snprintf(p, sizeof p, "/proc/self/task/%ld/stat", tid);
    int fd = open(p, O_RDONLY);
    ssize_t sn = fd >= 0 ? read(fd, buf, sizeof buf) : -1;
    if (fd >= 0) close(fd);
    snprintf(p, sizeof p, "/proc/self/task/%ld/comm", tid);
    fd = open(p, O_RDONLY);
    ssize_t cn = fd >= 0 ? read(fd, buf, sizeof buf) : -1;
    if (fd >= 0) close(fd);

    printf("proctask tids=%d isdir=%d stat=%d comm=%d\n", tids >= 1, isdir, sn > 0, cn > 0); // 1 1 1 1
    return 0;
}
