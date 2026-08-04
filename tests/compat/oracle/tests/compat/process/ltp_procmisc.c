// prctl / nanosleep / sched_getaffinity / read error-path semantics — LTP prctl02/prctl03/nanosleep02/
// sched_getaffinity01/read02 surface. Deterministic booleans only (no raw cpu counts / remaining-time
// values, which legitimately vary), oracle-diffed hl-vs-native on both arches.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <time.h>
#include <unistd.h>

int main(void) {
    // ---- prctl ----
    // PR_SET_NAME / PR_GET_NAME round-trip (prctl03-style: a real, supported option).
    prctl(PR_SET_NAME, "hlprobe");
    char nm[16] = {0};
    prctl(PR_GET_NAME, nm);
    printf("prctl name: %s\n", nm);

    // PR_GET_DUMPABLE returns 0 or 1.
    int dmp = prctl(PR_GET_DUMPABLE);
    printf("prctl dumpable in {0,1}: %d\n", dmp == 0 || dmp == 1);

    // prctl with a bogus option -> EINVAL (prctl02).
    errno = 0;
    int bad = prctl(-1, 0, 0, 0, 0);
    printf("prctl bad option EINVAL: ret=%d ok=%d\n", bad, bad < 0 && errno == EINVAL);

    // ---- nanosleep ----
    // normal short sleep completes -> 0.
    struct timespec req = {0, 1000000}, rem = {0, 0}; // 1ms
    int ns0 = nanosleep(&req, &rem);
    printf("nanosleep ok: ret=%d\n", ns0);

    // tv_nsec out of range -> EINVAL.
    errno = 0;
    struct timespec bad_ts = {0, 1000000000};
    int ns1 = nanosleep(&bad_ts, NULL);
    printf("nanosleep EINVAL nsec: ret=%d ok=%d\n", ns1, ns1 < 0 && errno == EINVAL);

    // negative tv_sec -> EINVAL.
    errno = 0;
    struct timespec neg_ts = {-1, 0};
    int ns2 = nanosleep(&neg_ts, NULL);
    printf("nanosleep EINVAL negsec: ret=%d ok=%d\n", ns2, ns2 < 0 && errno == EINVAL);

    // ---- sched_getaffinity ----
    cpu_set_t set;
    CPU_ZERO(&set);
    int sr = sched_getaffinity(0, sizeof set, &set);
    printf("sched_getaffinity ok: ret=%d cpus_ge1=%d\n", sr, CPU_COUNT(&set) >= 1);
    // a cpusetsize too small to hold the kernel's cpumask -> EINVAL.
    errno = 0;
    int sr2 = sched_getaffinity(0, 1, &set);
    printf("sched_getaffinity small EINVAL: ret=%d ok=%d\n", sr2, sr2 < 0 && errno == EINVAL);

    // ---- read error paths (read02) ----
    // read from a fd opened O_WRONLY -> EBADF.
    int w = open("/tmp/ltp_read02.tmp", O_WRONLY | O_CREAT | O_TRUNC, 0644);
    char b[4];
    errno = 0;
    long rw = read(w, b, 4);
    printf("read wronly EBADF: ret=%ld ok=%d\n", rw, rw < 0 && errno == EBADF);
    close(w);
    unlink("/tmp/ltp_read02.tmp");

    // read from a directory fd -> EISDIR.
    int d = open("/tmp", O_RDONLY);
    errno = 0;
    long rd = read(d, b, 4);
    printf("read dir EISDIR: ret=%ld ok=%d\n", rd, rd < 0 && errno == EISDIR);
    close(d);

    // read into a bad buffer pointer -> EFAULT.
    int z = open("/dev/zero", O_RDONLY);
    errno = 0;
    long rf = read(z, (void *)0x8, 16);
    printf("read EFAULT: ret=%ld ok=%d\n", rf, rf < 0 && errno == EFAULT);
    close(z);

    // read on a plain bad fd -> EBADF.
    errno = 0;
    long rb = read(400, b, 4);
    printf("read badfd EBADF: ret=%ld ok=%d\n", rb, rb < 0 && errno == EBADF);

    return 0;
}
