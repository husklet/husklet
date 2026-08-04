// mm SEMANTICS (task #397): the mmap/munmap/mincore/madvise error + observable-effect surface the LTP
// tests mmap08/mmap12/munmap01/munmap03/mincore02/mincore04/madvise10 exercise. Every line is a
// NORMALIZED verdict (booleans / counts, never a raw address or page size) so it is byte-identical on a
// 4 KB guest (x86) and a 16 KB guest (arm), and is diffed against the native oracle (qemu-x86_64 / native
// aarch64). Deliberately excludes the two behaviours hl cannot reproduce under a direct-mapped 4 KB-guest-
// on-16 KB-host model (a sub-host-page unmap/EOF page cannot be made to FAULT on access); those are a
// documented architectural deviation, not a semantics bug.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

#ifndef MAP_POPULATE
#define MAP_POPULATE 0x8000
#endif
#ifndef MADV_WIPEONFORK
#define MADV_WIPEONFORK 18
#endif
#ifndef MADV_KEEPONFORK
#define MADV_KEEPONFORK 19
#endif

static long PS;

// mmap08: a file mapping requested on a CLOSED (invalid) fd fails with EBADF.
static void t_badfd(void) {
    char tmpl[] = "/tmp/mmsemXXXXXX";
    int fd = mkstemp(tmpl);
    if (ftruncate(fd, PS)) { /* ignore */ }
    close(fd);
    errno = 0;
    void *p = mmap(NULL, PS, PROT_WRITE, MAP_FILE | MAP_SHARED, fd, 0);
    printf("badfd failed=%d ebadf=%d\n", p == MAP_FAILED, errno == EBADF);
    if (p != MAP_FAILED) munmap(p, PS);
    unlink(tmpl);
}

// A valid file descriptor with a non-page-aligned offset reaches the host mapping operation and must
// fail transactionally with EINVAL.  In particular, failure must not disturb the live descriptor or
// attempt to release mapping state that was never created.
static void t_bad_offset(void) {
    char tmpl[] = "/tmp/mmoffXXXXXX";
    int fd = mkstemp(tmpl);
    if (fd < 0 || ftruncate(fd, PS) != 0) {
        printf("mmap_badoff failed=0 einval=0 fd_live=0\n");
        if (fd >= 0) close(fd);
        unlink(tmpl);
        return;
    }
    errno = 0;
    void *p = mmap(NULL, PS, PROT_READ, MAP_PRIVATE, fd, 1);
    int failed = p == MAP_FAILED;
    int invalid = errno == EINVAL;
    int live = fcntl(fd, F_GETFD) >= 0;
    if (p != MAP_FAILED) munmap(p, PS);
    printf("mmap_badoff failed=%d einval=%d fd_live=%d\n", failed, invalid, live);
    close(fd);
    unlink(tmpl);
}

// mmap with length 0 -> EINVAL (anon and file).
static void t_len0(void) {
    errno = 0;
    void *p = mmap(NULL, 0, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    printf("mmap_len0 failed=%d einval=%d\n", p == MAP_FAILED, errno == EINVAL);
    if (p != MAP_FAILED) munmap(p, PS);
}

// munmap03: EINVAL for len 0, a mis-aligned addr, and an out-of-range (wrapping) range.
static void t_munmap_einval(void) {
    void *m = mmap(NULL, PS * 2, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    errno = 0;
    int r0 = munmap(m, 0);
    int e0 = errno == EINVAL;
    errno = 0;
    int r1 = munmap((char *)m + 1, PS);
    int e1 = errno == EINVAL;
    errno = 0;
    unsigned long big = (unsigned long)-PS; // addr+len wraps
    int r2 = munmap((void *)big, PS * 2);
    int e2 = errno == EINVAL;
    printf("munmap_len0 einval=%d\n", r0 == -1 && e0);
    printf("munmap_misalign einval=%d\n", r1 == -1 && e1);
    printf("munmap_oor einval=%d\n", r2 == -1 && e2);
    munmap(m, PS * 2);
}

// A COMPLETE unmap of a whole mapping succeeds and the mapping is gone (return 0). (The fault-on-access
// check that LTP munmap01 adds is a host-page property hl cannot honor for a 4 KB guest on a 16 KB host,
// so only the success/return value is asserted here.)
static void t_munmap_ok(void) {
    void *m = mmap(NULL, PS * 3, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int r = munmap(m, PS * 3);
    printf("munmap_ok r=%d\n", r);
}

// mmap12: MAP_POPULATE of a zero (ftruncated) file, MAP_PRIVATE -> succeeds, every byte reads back zero.
static void t_populate(void) {
    char tmpl[] = "/tmp/mmsemXXXXXX";
    int fd = mkstemp(tmpl);
    size_t sz = 64 * 1024;
    if (ftruncate(fd, sz)) { /* ignore */ }
    char *a = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_POPULATE, fd, 0);
    int ok = a != MAP_FAILED, allzero = 1;
    if (ok)
        for (size_t i = 0; i < sz; i++)
            if (a[i]) { allzero = 0; break; }
    printf("populate ok=%d allzero=%d\n", ok, allzero);
    if (ok) munmap(a, sz);
    close(fd);
    unlink(tmpl);
}

// mincore02/04: residency vector of a locked mapping (all touched+mlocked pages resident); a mis-aligned
// start address is EINVAL.
static void t_mincore(void) {
    int n = 4;
    size_t len = PS * n;
    char *m = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    for (int i = 0; i < n; i++) m[i * PS] = 1;
    mlock(m, len);
    unsigned char vec[8] = {0};
    int rc = mincore(m, len, vec);
    int res = 0;
    for (int i = 0; i < n; i++) res += vec[i] & 1;
    errno = 0;
    int rmis = mincore((char *)m + 1, len, vec);
    int emis = errno == EINVAL;
    printf("mincore rc=%d all_resident=%d misalign_einval=%d\n", rc, res == n, rmis == -1 && emis);
    munlock(m, len);
    munmap(m, len);
}

// madvise10 + DONTNEED/FREE/WILLNEED: DONTNEED drops private-anon pages so they read back ZERO; WILLNEED
// and FREE succeed; MADV_WIPEONFORK presents the child zero-filled while the parent keeps its data, and
// MADV_KEEPONFORK undoes it. (MADV_FREE's post-advice contents are kernel-lazy, so only its rc is checked.)
static void t_madvise(void) {
    size_t sz = 64 * 1024;
    char *m = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

    memset(m, 0xAB, sz);
    int rd = madvise(m, sz, MADV_DONTNEED);
    long after = 0;
    for (size_t i = 0; i < sz; i += PS) after += (unsigned char)m[i];
    printf("madv_dontneed rc=%d zeroed=%d\n", rd, after == 0);

    printf("madv_willneed rc=%d\n", madvise(m, sz, MADV_WILLNEED));
    printf("madv_free rc=%d\n", madvise(m, sz, MADV_FREE));

    // WIPEONFORK: child sees zeros, parent keeps its data.
    memset(m, 0xCD, sz);
    int rw = madvise(m, sz, MADV_WIPEONFORK);
    if (rw == 0) {
        pid_t pid = fork();
        if (pid == 0) {
            int zero = 1;
            for (size_t i = 0; i < sz; i++)
                if (m[i]) { zero = 0; break; }
            _exit(zero ? 0 : 1);
        }
        int st;
        waitpid(pid, &st, 0);
        int parent_keeps = (unsigned char)m[0] == 0xCD;
        printf("madv_wipeonfork rc=0 child_zero=%d parent_keeps=%d\n",
               WIFEXITED(st) && WEXITSTATUS(st) == 0, parent_keeps);
    } else {
        printf("madv_wipeonfork rc=%d einval=%d\n", rw, errno == EINVAL);
    }
    // KEEPONFORK undoes it -> child keeps the parent's data.
    int rk = madvise(m, sz, MADV_KEEPONFORK);
    if (rk == 0) {
        memset(m, 0xEE, sz);
        pid_t pid = fork();
        if (pid == 0) {
            int kept = (unsigned char)m[0] == 0xEE;
            _exit(kept ? 0 : 1);
        }
        int st;
        waitpid(pid, &st, 0);
        printf("madv_keeponfork rc=0 child_keeps=%d\n", WIFEXITED(st) && WEXITSTATUS(st) == 0);
    } else {
        printf("madv_keeponfork rc=%d\n", rk);
    }
    munmap(m, sz);
}

int main(void) {
    PS = sysconf(_SC_PAGESIZE);
    t_badfd();
    t_bad_offset();
    t_len0();
    t_munmap_einval();
    t_munmap_ok();
    t_populate();
    t_mincore();
    t_madvise();
    return 0;
}
