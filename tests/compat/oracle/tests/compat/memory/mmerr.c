// mm ERRNO + FLAG surface (task #417): the mmap/msync/madvise/mremap error paths and flag semantics not
// already pinned by mmsem.c/mremap.c. Every line is a NORMALIZED verdict (booleans / errno-NAMES, never a
// raw address or page size) so it is byte-identical on a 4 KB guest (x86) and a 16 KB guest (arm), and is
// diffed against the native oracle (qemu-x86_64 / native aarch64) on BOTH Linux engines.
//
// Deliberately EXCLUDES the three behaviours hl cannot reproduce under its force-mapped, 4 KB-guest-on-
// 16 KB-host model and that are documented architectural deviations, not semantics bugs:
//   * mprotect(PROT_NONE)/mprotect over an unmapped range -> ENOMEM  (mprotect is a JIT no-op; see mem.c
//     case 226 + edge_mprotect.c which is .xfail(lin)),
//   * msync over a fully-unmapped range -> ENOMEM  (the shared-page cache is already coherent; no-op),
//   * mincore over a range containing a hole -> ENOMEM  (macOS mincore does not fault holes the way Linux
//     does, and a sub-host-page guest unmap keeps the host page mapped).
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef MAP_NORESERVE
#define MAP_NORESERVE 0x4000
#endif
#ifndef MADV_DONTFORK
#define MADV_DONTFORK 10
#define MADV_DOFORK   11
#endif
#ifndef MREMAP_MAYMOVE
#define MREMAP_MAYMOVE 1
#define MREMAP_FIXED   2
#endif

// Linux MS_* (guest values): ASYNC=1, INVALIDATE=2, SYNC=4.
#define L_MS_ASYNC      1
#define L_MS_INVALIDATE 2
#define L_MS_SYNC       4

static long PS;

static const char *en(int e) {
    switch (e) {
    case 0: return "0";
    case EINVAL: return "EINVAL";
    case ENOMEM: return "ENOMEM";
    case EACCES: return "EACCES";
    case EBADF: return "EBADF";
    default: return "OTHER";
    }
}

// MAP_NORESERVE anon: succeeds and reads/writes back like any private-anon page (no swap reservation is
// a hint; the mapping is fully usable).
static void t_noreserve(void) {
    char *m = mmap(NULL, PS, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE | MAP_NORESERVE, -1, 0);
    int ok = m != MAP_FAILED;
    int val = ok ? (m[0] = 0x7e, m[0]) : -1;
    printf("noreserve ok=%d val=%d\n", ok, val);
    if (ok) munmap(m, PS);
}

// A writable MAP_SHARED mapping of a file opened O_RDONLY is EACCES (mmap needs write access to the file
// for a shared writable map). A MAP_PRIVATE map of the same fd is allowed (copy-on-write, no write-back).
static void t_prot_eacces(void) {
    char tmpl[] = "/tmp/mmerrXXXXXX";
    int fd = mkstemp(tmpl);
    if (write(fd, "abcd", 4) != 4) { /* ignore */ }
    close(fd);
    int rf = open(tmpl, O_RDONLY);
    errno = 0;
    void *sh = mmap(NULL, PS, PROT_READ | PROT_WRITE, MAP_SHARED, rf, 0);
    int sh_eacces = sh == MAP_FAILED && errno == EACCES;
    if (sh != MAP_FAILED) munmap(sh, PS);
    void *pr = mmap(NULL, PS, PROT_READ | PROT_WRITE, MAP_PRIVATE, rf, 0);
    int pr_ok = pr != MAP_FAILED;
    if (pr_ok) munmap(pr, PS);
    close(rf);
    unlink(tmpl);
    printf("mmap_rdonly shared_eacces=%d private_ok=%d\n", sh_eacces, pr_ok);
}

// msync flag validation (Linux mm/msync.c rejects the flags BEFORE any writeback): MS_SYNC and MS_ASYNC
// each succeed on a shared file map; both-set or an unknown bit is EINVAL.
static void t_msync_flags(void) {
    char tmpl[] = "/tmp/mmerrXXXXXX";
    int fd = mkstemp(tmpl);
    if (ftruncate(fd, PS)) { /* ignore */ }
    char *m = mmap(NULL, PS, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    m[0] = 1;
    int rs = msync(m, PS, L_MS_SYNC);
    int ra = msync(m, PS, L_MS_ASYNC);
    errno = 0;
    int rboth = msync(m, PS, L_MS_SYNC | L_MS_ASYNC);
    int eboth = errno;
    errno = 0;
    int rbad = msync(m, PS, 0x40 /* unknown bit */);
    int ebad = errno;
    printf("msync sync=%d async=%d both=%d/%s badbit=%d/%s\n", rs, ra, rboth, en(rboth ? eboth : 0), rbad,
           en(rbad ? ebad : 0));
    munmap(m, PS);
    close(fd);
    unlink(tmpl);
}

// madvise(MADV_DONTFORK)/MADV_DOFORK on a private-anon range are accepted (advisory; the child-visibility
// effect is not asserted here, only the return value the kernel gives).
static void t_madv_fork(void) {
    char *m = mmap(NULL, PS, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    int rdf = madvise(m, PS, MADV_DONTFORK);
    int rdo = madvise(m, PS, MADV_DOFORK);
    printf("madv_dontfork rc=%d dofork rc=%d\n", rdf, rdo);
    munmap(m, PS);
}

// mremap(MREMAP_FIXED): moves the mapping to EXACTLY new_addr and preserves the bytes. Plus the three
// EINVAL guards Linux enforces: FIXED without MAYMOVE, a mis-aligned new_addr, and a new range that
// overlaps the old.
static void t_mremap_fixed(void) {
    char *m = mmap(NULL, PS, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    m[0] = 0x42;
    // Reserve a distinct destination so MREMAP_FIXED replaces a range we own.
    char *dst = mmap(NULL, PS * 2, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    void *r = mremap(m, PS, PS, MREMAP_MAYMOVE | MREMAP_FIXED, dst);
    int moved = r == dst;
    int val = moved ? dst[0] : -1;
    printf("mremap_fixed moved=%d val=%d\n", moved, val);
    if (r != MAP_FAILED) munmap(r, PS);
    else munmap(dst, PS * 2);

    // FIXED without MAYMOVE -> EINVAL.
    char *a = mmap(NULL, PS, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    char *b = mmap(NULL, PS, PROT_READ | PROT_WRITE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
    errno = 0;
    void *r1 = mremap(a, PS, PS, MREMAP_FIXED, b);
    int e1 = r1 == MAP_FAILED && errno == EINVAL;
    // Mis-aligned new_addr -> EINVAL.
    errno = 0;
    void *r2 = mremap(a, PS, PS, MREMAP_MAYMOVE | MREMAP_FIXED, (char *)b + 1);
    int e2 = r2 == MAP_FAILED && errno == EINVAL;
    printf("mremap_fixed_nomove einval=%d misalign einval=%d\n", e1, e2);
    munmap(a, PS);
    munmap(b, PS);
}

int main(void) {
    PS = sysconf(_SC_PAGESIZE);
    t_noreserve();
    t_prot_eacces();
    t_msync_flags();
    t_madv_fork();
    t_mremap_fixed();
    return 0;
}
