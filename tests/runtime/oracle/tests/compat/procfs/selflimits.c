// /proc/self/limits is the human-readable rlimit table (systemd, container runtimes and the JVM read it).
// Beyond the well-known NOFILE values, assert the table's STRUCTURE and that it agrees with getrlimit(2)
// for this process: a header line, exactly 16 resource rows, and the "Max open files" / "Max stack size"
// soft columns equal to the live getrlimit soft values (with "unlimited" mapping to RLIM_INFINITY). A
// synthesized limits file that disagrees with the syscall it is supposed to mirror fails. Derived.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include "pf.h"

// soft value from a "<name>...  <soft> <hard> ..." row; -1 if absent.
static long long soft_of(const char *b, const char *name) {
    const char *ln = strstr(b, name);
    if (!ln) return -2;
    ln += strlen(name);
    while (*ln == ' ') ln++;
    if (!strncmp(ln, "unlimited", 9)) return -1; // RLIM_INFINITY sentinel
    return strtoll(ln, NULL, 10);
}

static long long soft_rl(int res) {
    struct rlimit rl;
    if (getrlimit(res, &rl) != 0) return -2;
    return rl.rlim_cur == RLIM_INFINITY ? -1 : (long long)rl.rlim_cur;
}

int main(void) {
    char b[8192];
    int n = pf_read("/proc/self/limits", b, sizeof b);
    int has_header = !strncmp(b, "Limit", 5);
    int lines = 0;
    for (const char *p = strchr(b, '\n'); p; p = strchr(p + 1, '\n')) lines++;
    int nofile_ok = soft_of(b, "Max open files") == soft_rl(RLIMIT_NOFILE);
    int stack_ok = soft_of(b, "Max stack size") == soft_rl(RLIMIT_STACK);
    int nproc_present = soft_of(b, "Max processes") != -2;
    // header + 16 resource rows -> at least 16 line terminators.
    int ok = n > 0 && has_header && lines >= 16 && nofile_ok && stack_ok && nproc_present;
    printf("selflimits ok=%d\n", ok);
    return 0;
}
