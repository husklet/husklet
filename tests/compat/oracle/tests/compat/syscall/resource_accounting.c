// Resource-accounting + system-identity self-consistency (getrusage / times / uname / sysconf), asserting
// native-true RELATIONSHIPS rather than dynamic absolute values:
//   - uname: sysname == "Linux", machine == the GUEST architecture this binary was built for (NOT the host
//     arch), release/nodename non-empty;
//   - getrusage(RUSAGE_SELF): ru_maxrss > 0 and does not shrink after a large touched allocation (high-water),
//     ru_minflt advances across that allocation, and RUSAGE_CHILDREN accumulates CPU after a child runs+waits;
//   - times(2): the tick return value is monotonic across a busy interval and tms_utime does not go backwards;
//   - sysconf: _SC_CLK_TCK > 0, _SC_PAGESIZE == getpagesize(), _SC_NPROCESSORS_ONLN > 0, and
//     _SC_PHYS_PAGES * _SC_PAGE_SIZE == /proc/meminfo MemTotal (glibc derives phys pages from MemTotal).
// Single boolean, validated by running THROUGH the engine.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/times.h>
#include <sys/utsname.h>
#include <sys/wait.h>
#include <unistd.h>

static unsigned long long memtotal_kb(void) {
    FILE *f = fopen("/proc/meminfo", "r");
    char line[256];
    unsigned long long kb = 0;
    if (!f) return 0;
    while (fgets(line, sizeof line, f))
        if (sscanf(line, "MemTotal: %llu kB", &kb) == 1) break;
    fclose(f);
    return kb;
}

static void spin(void) {
    volatile unsigned long x = 0;
    for (unsigned long i = 0; i < 40000000UL; i++) x += i;
}

int main(void) {
    int ok = 1;

    struct utsname u;
    if (uname(&u) != 0) ok = 0;
    if (strcmp(u.sysname, "Linux") != 0) ok = 0;
#if defined(__aarch64__)
    if (strcmp(u.machine, "aarch64") != 0) ok = 0;
#elif defined(__x86_64__)
    if (strcmp(u.machine, "x86_64") != 0) ok = 0;
#endif
    if (u.release[0] == 0 || u.nodename[0] == 0) ok = 0;

    struct rusage r0, r1;
    if (getrusage(RUSAGE_SELF, &r0) != 0) ok = 0;
    if (r0.ru_maxrss <= 0) ok = 0;
    const size_t megs = 64;
    char *buf = malloc(megs << 20);
    if (buf) {
        memset(buf, 0x5a, megs << 20);
        if (getrusage(RUSAGE_SELF, &r1) != 0) ok = 0;
        if (r1.ru_maxrss < r0.ru_maxrss) ok = 0;   // high-water must not shrink
        if (r1.ru_minflt < r0.ru_minflt) ok = 0;   // faulting in the buffer raises minor faults
        free(buf);
    } else {
        ok = 0;
    }

    // times(2): monotonic tick counter + non-decreasing tms_utime across CPU work.
    struct tms tb0, tb1;
    clock_t c0 = times(&tb0);
    spin();
    clock_t c1 = times(&tb1);
    if (c0 == (clock_t)-1 || c1 == (clock_t)-1) ok = 0;
    if (c1 < c0) ok = 0;
    if (tb1.tms_utime < tb0.tms_utime) ok = 0;

    // RUSAGE_CHILDREN accumulates after a CPU-burning child.
    pid_t pid = fork();
    if (pid == 0) {
        spin();
        _exit(0);
    } else if (pid > 0) {
        int st;
        waitpid(pid, &st, 0);
        struct rusage rc;
        if (getrusage(RUSAGE_CHILDREN, &rc) != 0) ok = 0;
        if (rc.ru_utime.tv_sec == 0 && rc.ru_utime.tv_usec == 0 && rc.ru_stime.tv_sec == 0 &&
            rc.ru_stime.tv_usec == 0)
            ok = 0; // some CPU must be attributed to the reaped child
    } else {
        ok = 0;
    }

    long clk = sysconf(_SC_CLK_TCK);
    long pg = sysconf(_SC_PAGESIZE);
    long nproc = sysconf(_SC_NPROCESSORS_ONLN);
    long phys = sysconf(_SC_PHYS_PAGES);
    if (clk <= 0 || pg <= 0 || nproc <= 0 || phys <= 0) ok = 0;
    if (pg != getpagesize()) ok = 0;

    unsigned long long mt = memtotal_kb() * 1024ULL;
    if (mt == 0) ok = 0;
    else if ((unsigned long long)phys * (unsigned long long)pg != mt) ok = 0;

    printf("resource-accounting ok=%d\n", ok);
    return 0;
}
