// EDGE / regress: the x86-legacy time-setter family that has NO aarch64 canonical syscall number and so
// used to fall through hl's x86->canonical normalization to ENOSYS:
//   utime(2)=132, utimes(2)=235, futimesat(2)=261  (+ the modern utimensat(2) they are rewritten onto).
// hl rewrites each to utimensat(dirfd, path, timespec[2], flags), converting struct utimbuf / struct
// timeval[2] -> struct timespec[2], and mapping NULL times to "set to now". This guest exercises, for each:
//   * explicit times (distinct atime != mtime, so both fields are proven to flow through independently),
//   * NULL times (=> current time; checked as "recent", not an exact value, to stay deterministic),
// plus utimensat's UTIME_OMIT (leave one field) and UTIME_NOW (bump one field to now) which the legacy forms
// cannot express but the modern target must honor. All checks reduce to booleans + second-granularity time
// compares (nanosecond FS granularity differs across the hl/oracle filesystems, so we compare seconds only),
// so stdout is byte-identical to the native/qemu oracle. Prints "utimes-family OK" iff every check passes.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>
#include <utime.h>

#define A1 1000000000L // distinct fixed epoch seconds for atime/mtime so both fields are observable
#define M1 1200000000L
#define A2 1300000000L
#define M2 1400000000L

static int mk(const char *p) { // create/truncate the target file
    int fd = open(p, O_CREAT | O_TRUNC | O_WRONLY, 0644);
    if (fd < 0) return -1;
    close(fd);
    return 0;
}
static int at_sec(const char *p, long *a, long *m) { // stat -> atime/mtime seconds
    struct stat st;
    if (stat(p, &st) < 0) return -1;
    *a = (long)st.st_atime;
    *m = (long)st.st_mtime;
    return 0;
}
static int recent(long t, long now) { return t >= now - 5 && t <= now + 30; } // "set to now" window

int main(void) {
    const char *path = "/tmp/hl_utimes_family";
    const char *rel = "hl_utimes_rel";
    long a, m, now;
    int ok = 1;

    // 1. utime() with explicit utimbuf.
    if (mk(path)) { printf("utimes-family FAIL mk\n"); return 1; }
    struct utimbuf ub = {.actime = A1, .modtime = M1};
    if (utime(path, &ub) < 0 || at_sec(path, &a, &m) < 0) ok = 0;
    int c_utime = (a == A1 && m == M1);

    // 2. utime(NULL) -> now.
    now = (long)time(NULL);
    if (utime(path, NULL) < 0 || at_sec(path, &a, &m) < 0) ok = 0;
    int c_utime_now = recent(a, now) && recent(m, now);

    // 3. utimes() with explicit timeval[2] (microseconds present; only seconds are compared).
    struct timeval tv[2] = {{.tv_sec = A2, .tv_usec = 123456}, {.tv_sec = M2, .tv_usec = 654321}};
    if (utimes(path, tv) < 0 || at_sec(path, &a, &m) < 0) ok = 0;
    int c_utimes = (a == A2 && m == M2);

    // 4. utimes(NULL) -> now.
    now = (long)time(NULL);
    if (utimes(path, NULL) < 0 || at_sec(path, &a, &m) < 0) ok = 0;
    int c_utimes_now = recent(a, now) && recent(m, now);

    // 5. futimesat(AT_FDCWD, path, timeval[2]).
    struct timeval tv2[2] = {{.tv_sec = A1, .tv_usec = 1}, {.tv_sec = M2, .tv_usec = 2}};
    if (futimesat(AT_FDCWD, path, tv2) < 0 || at_sec(path, &a, &m) < 0) ok = 0;
    int c_fut_cwd = (a == A1 && m == M2);

    // 6. futimesat(dirfd, relpath, timeval[2]) -- real directory fd + relative path.
    int dfd = open("/tmp", O_RDONLY | O_DIRECTORY);
    char rp[64];
    snprintf(rp, sizeof rp, "/tmp/%s", rel);
    int c_fut_dir = 0;
    if (dfd >= 0 && mk(rp) == 0) {
        struct timeval tv3[2] = {{.tv_sec = A2, .tv_usec = 0}, {.tv_sec = M1, .tv_usec = 0}};
        if (futimesat(dfd, rel, tv3) == 0 && at_sec(rp, &a, &m) == 0) c_fut_dir = (a == A2 && m == M1);
        close(dfd);
        unlink(rp);
    }

    // 7. utimensat UTIME_OMIT: set atime only (A1), leave mtime as-is (currently M2 from step 5... but step 6
    //    used a different file). Re-seed the main file to a known state, then OMIT mtime.
    struct timeval seed[2] = {{.tv_sec = A2, .tv_usec = 0}, {.tv_sec = M2, .tv_usec = 0}};
    utimes(path, seed);
    struct timespec om[2] = {{.tv_sec = A1, .tv_nsec = 0}, {.tv_nsec = UTIME_OMIT}};
    if (utimensat(AT_FDCWD, path, om, 0) < 0 || at_sec(path, &a, &m) < 0) ok = 0;
    int c_omit = (a == A1 && m == M2); // atime changed, mtime untouched

    // 8. utimensat UTIME_NOW on mtime, OMIT atime.
    now = (long)time(NULL);
    struct timespec nw[2] = {{.tv_nsec = UTIME_OMIT}, {.tv_nsec = UTIME_NOW}};
    if (utimensat(AT_FDCWD, path, nw, 0) < 0 || at_sec(path, &a, &m) < 0) ok = 0;
    int c_now = (a == A1) && recent(m, now); // atime untouched, mtime -> now

    unlink(path);

    ok = ok && c_utime && c_utime_now && c_utimes && c_utimes_now && c_fut_cwd && c_fut_dir && c_omit && c_now;
    if (ok) {
        printf("utimes-family OK\n");
        return 0;
    }
    printf("utimes-family FAIL utime=%d utime_now=%d utimes=%d utimes_now=%d fut_cwd=%d fut_dir=%d omit=%d now=%d\n",
           c_utime, c_utime_now, c_utimes, c_utimes_now, c_fut_cwd, c_fut_dir, c_omit, c_now);
    return 1;
}
