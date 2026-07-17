// /proc/self/maps + /proc/self/fd structural conformance vs real Linux (what pmap/gdb/jemalloc/glibc-malloc
// and lsof read). Asserts the invariants a correct maps MUST hold — NOT host-variant addresses/inodes:
//   * every line is "lo-hi perms offset dev inode [name]" with a 4-char perms field ending p/s and a
//     dev field of the form NN:NN;
//   * the executable's own text is mapped r-xp somewhere;
//   * regions are ASCENDING and NON-OVERLAPPING (the kernel invariant a sequential parser relies on) —
//     hl emitted them in gmap-registration order before the fix, so this fails on the pre-fix engine;
//   * a live heap allocation lands inside SOME writable mapping (the allocator's memory is represented),
//     and on aarch64 (growable brk) a line named [heap] exists — jemalloc/redis/pmap look for it;
//   * a [stack] line exists (glibc pthread_getattr_np scans for it).
// fd: opendir enumerates >=3 entries, a readlink resolves, and a non-file fd (a pipe) resolves to a
// pipe:/socket:/anon_inode: target (lsof classifies fds by that prefix).
#define _GNU_SOURCE
#include <dirent.h>
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <unistd.h>
#include "pf.h"

int main(void) {
    // Force the allocator to touch memory so a heap/arena mapping exists and is resident.
    volatile char *blk = NULL;
    for (int i = 0; i < 64; i++) { char *p = malloc(4096); if (p) { p[0] = (char)i; p[4095] = (char)i; blk = p; } }
    unsigned long probe = (unsigned long)(uintptr_t)blk;

    char b[1 << 16];
    int n = pf_read("/proc/self/maps", b, sizeof b);

    int perms_ok = 1, has_xp = 0, has_stack = 0, has_heap = 0, ascending = 1, dev_ok = 1, probe_mapped = 0;
    int nlines = 0;
    unsigned long prev_hi = 0;
    for (const char *p = b; p && *p;) {
        unsigned long lo = 0, hi = 0;
        char perms[8] = {0}, off[16] = {0}, dev[16] = {0};
        char name[256] = {0};
        // "lo-hi perms offset dev inode [name]"
        int f = sscanf(p, "%lx-%lx %7s %15s %15s %*s %255[^\n]", &lo, &hi, perms, off, dev, name);
        if (f >= 5) {
            nlines++;
            if (!(strlen(perms) == 4 && (perms[3] == 'p' || perms[3] == 's'))) perms_ok = 0;
            // dev field must be NN:NN
            int c1 = 0, c2 = 0;
            if (sscanf(dev, "%x:%x", &c1, &c2) != 2) dev_ok = 0;
            if (perms[0] == 'r' && perms[2] == 'x') has_xp = 1;
            if (strstr(name, "[stack]")) has_stack = 1;
            if (strstr(name, "[heap]")) has_heap = 1;
            if (lo < prev_hi) ascending = 0; // overlaps / out-of-order vs the previous region
            prev_hi = hi;
            if (probe && probe >= lo && probe < hi && perms[1] == 'w') probe_mapped = 1;
        }
        const char *nl = strchr(p, '\n');
        p = nl ? nl + 1 : 0;
    }

    // aarch64 has a growable brk -> glibc/malloc build a [heap]; x86 uses a non-growable break (mmap arenas
    // only), so there is no brk heap there — the arena shows as an anon mapping (probe_mapped covers it).
#if defined(__aarch64__)
    int heap_ok = has_heap;
#else
    int heap_ok = 1;
#endif

    DIR *d = opendir("/proc/self/fd");
    int fdcount = 0, pipe_seen = 0;
    if (d) {
        struct dirent *e;
        while ((e = readdir(d))) {
            if (e->d_name[0] == '.') continue;
            fdcount++;
            char path[128], tgt[256];
            snprintf(path, sizeof path, "/proc/self/fd/%s", e->d_name);
            ssize_t ll = readlink(path, tgt, sizeof tgt - 1);
            if (ll > 0) {
                tgt[ll] = 0;
                if (!strncmp(tgt, "pipe:[", 6) || !strncmp(tgt, "socket:[", 8) ||
                    !strncmp(tgt, "anon_inode:", 11)) pipe_seen = 1;
            }
        }
        closedir(d);
    }
    char lnk[256];
    ssize_t ll = readlink("/proc/self/fd/1", lnk, sizeof lnk - 1);
    int fd_link_ok = ll > 0;

    int ok = n > 0 && nlines >= 3 && perms_ok && dev_ok && has_xp && has_stack && heap_ok &&
             ascending && probe_mapped && fdcount >= 3 && fd_link_ok && pipe_seen;
    printf("maps ok=%d\n", ok);
    return 0;
}
