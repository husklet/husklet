// /proc/self/maps must name the files the guest mapped, and must stay non-overlapping when a MAP_FIXED
// replaces part of an earlier reservation. That is the shape every dynamic guest produces -- ld.so reserves
// a library's whole span, then MAP_FIXEDs each PT_LOAD inside it -- so it is exercised here with an explicit
// reservation + MAP_FIXED, which a static guest can build and which does not depend on a loader being
// present. pf-maps already asserts the global ordering invariant; before this fixture nothing reached the
// case where two rows genuinely collided, and every mapped file rendered as an unnamed anonymous row.
//
// Asserted, all host-invariant: rows ascending and non-overlapping; a mapped file's row carries a non-zero
// dev:inode AND its pathname; the file offset column tracks the mmap offset; the MAP_FIXED sub-range is its
// own row rather than a hole inside the reservation's; and map_files/ readlinks to the same path.
#define _GNU_SOURCE
#include <dirent.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>

#include "pf.h"

#define PG 4096u

struct row {
    unsigned long lo, hi;
    char perms[8], dev[16], name[512];
    unsigned long long off, ino;
};
static struct row rows[4096];
static int nrow;

static void load(void) {
    char b[1 << 18];
    int n = pf_read("/proc/self/maps", b, sizeof b);
    nrow = 0;
    if (n <= 0) return;
    for (const char *p = b; p && *p && nrow < 4096;) {
        struct row *r = &rows[nrow];
        memset(r, 0, sizeof *r);
        if (sscanf(p, "%lx-%lx %7s %llx %15s %llu %511[^\n]", &r->lo, &r->hi, r->perms, &r->off, r->dev, &r->ino,
                   r->name) >= 6)
            nrow++;
        const char *nl = strchr(p, '\n');
        p = nl ? nl + 1 : 0;
    }
}

static const struct row *covering(unsigned long a) {
    for (int i = 0; i < nrow; i++)
        if (a >= rows[i].lo && a < rows[i].hi) return &rows[i];
    return NULL;
}

// A row naming `path` with a real dev:inode, whose offset column equals `off` at its start.
static int named(const struct row *r, const char *path, unsigned long long off, unsigned long a) {
    return r && r->ino != 0 && !strcmp(r->name, path) && r->off == off + (a - r->lo);
}

int main(void) {
    char exe[4096];
    ssize_t el = readlink("/proc/self/exe", exe, sizeof exe - 1);
    exe[el > 0 ? el : 0] = 0;
    int fd = open("/proc/self/exe", O_RDONLY);
    struct stat st;
    int stat_ok = fd >= 0 && fstat(fd, &st) == 0 && st.st_size >= (off_t)(8 * PG);

    // 1. A plain private file mapping at a non-zero offset.
    char *plain = MAP_FAILED;
    if (fd >= 0) plain = mmap(NULL, 2 * PG, PROT_READ, MAP_PRIVATE, fd, PG);

    // 2. A whole-span reservation, then a MAP_FIXED file mapping over its middle -- the loader's shape.
    char *span = mmap(NULL, 4 * PG, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    char *inner = MAP_FAILED;
    if (span != MAP_FAILED && fd >= 0)
        inner = mmap(span + PG, 2 * PG, PROT_READ, MAP_PRIVATE | MAP_FIXED, fd, 2 * PG);

    int maps_ok = plain != MAP_FAILED && span != MAP_FAILED && inner == span + PG;
    load();

    // Global invariant: ascending, non-overlapping. A reservation the MAP_FIXED was never subtracted from
    // shows up here as rows[i].lo < rows[i-1].hi.
    int ordered = 1;
    for (int i = 1; i < nrow; i++)
        if (rows[i].lo < rows[i - 1].hi) ordered = 0;

    int plain_named = maps_ok && named(covering((unsigned long)plain), exe, PG, (unsigned long)plain);
    int inner_named = maps_ok && named(covering((unsigned long)inner), exe, 2 * PG, (unsigned long)inner);
    // The MAP_FIXED range must be its OWN row: the row covering it must not extend below it.
    const struct row *ir = maps_ok ? covering((unsigned long)inner) : NULL;
    int inner_split = ir && ir->lo == (unsigned long)inner;
    // and the reservation's surviving head must be a separate anonymous row.
    const struct row *hr = maps_ok ? covering((unsigned long)span) : NULL;
    int head_anon = hr && hr->ino == 0 && hr->hi <= (unsigned long)inner;

    // map_files/ must readlink the same path for the mapped ranges.
    int mf_ok = 0;
    if (maps_ok && ir) {
        char p[256], tgt[4096];
        snprintf(p, sizeof p, "/proc/self/map_files/%lx-%lx", ir->lo, ir->hi);
        ssize_t r = readlink(p, tgt, sizeof tgt - 1);
        tgt[r > 0 ? r : 0] = 0;
        mf_ok = r > 0 && !strcmp(tgt, exe);
    }

    printf("setup=%d ordered=%d\n", stat_ok && maps_ok, ordered);
    printf("plain_named=%d inner_named=%d inner_split=%d head_anon=%d\n", plain_named, inner_named, inner_split,
           head_anon);
    printf("map_files_agrees=%d\n", mf_ok);

    // The perms column must follow the LIVE protection, not the one the mapping was created with. hl derived
    // the image rows from the program headers and gave every other row a flat rw-p, so a guest that
    // mprotect'd anything -- a JIT toggling RW/RX, a runtime auditing W^X, a RELRO check -- read a stale
    // answer. Round-trip one page the guest owns through all three states, then the executable's own text.
    char *pg1 = mmap(NULL, PG, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int prot_rw = 0, prot_ro = 0, prot_none = 0, prot_back = 0;
    if (pg1 != MAP_FAILED) {
        const struct row *r;
        load();
        r = covering((unsigned long)pg1);
        prot_rw = r && r->perms[0] == 'r' && r->perms[1] == 'w';
        if (mprotect(pg1, PG, PROT_READ) == 0) {
            load();
            r = covering((unsigned long)pg1);
            prot_ro = r && r->perms[0] == 'r' && r->perms[1] == '-';
        }
        if (mprotect(pg1, PG, PROT_NONE) == 0) {
            load();
            r = covering((unsigned long)pg1);
            prot_none = r && r->perms[0] == '-' && r->perms[1] == '-' && r->perms[2] == '-';
        }
        if (mprotect(pg1, PG, PROT_READ | PROT_WRITE) == 0) {
            load();
            r = covering((unsigned long)pg1);
            prot_back = r && r->perms[0] == 'r' && r->perms[1] == 'w';
        }
        munmap(pg1, PG);
    }
    printf("prot_rw=%d prot_ro=%d prot_none=%d prot_back=%d\n", prot_rw, prot_ro, prot_none, prot_back);

    // A JIT's own W^X toggle on a code page it owns: RX -> RW -> RX. Only the WRITE bit is asserted; the
    // engine tracks guest PROT_NONE and read-only ranges but not PROT_EXEC, so its rows under-report x on a
    // guest-created mapping (reported, not fixed). A row that claims w while the guest asked for RX is the
    // failure a W^X audit actually cares about, and that is what this pins.
    char *code = mmap(NULL, PG, PROT_READ | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    int wx_rx = 0, wx_rw = 0, wx_back = 0;
    if (code != MAP_FAILED) {
        const struct row *r;
        load();
        r = covering((unsigned long)code);
        wx_rx = r && r->perms[1] == '-';
        if (mprotect(code, PG, PROT_READ | PROT_WRITE) == 0) {
            load();
            r = covering((unsigned long)code);
            wx_rw = r && r->perms[1] == 'w';
        }
        if (mprotect(code, PG, PROT_READ | PROT_EXEC) == 0) {
            load();
            r = covering((unsigned long)code);
            wx_back = r && r->perms[1] == '-';
        }
        munmap(code, PG);
    }
    printf("wx_rx_not_writable=%d wx_rw_writable=%d wx_back_not_writable=%d\n", wx_rx, wx_rw, wx_back);
    if (plain != MAP_FAILED) munmap(plain, 2 * PG);
    if (span != MAP_FAILED) munmap(span, 4 * PG);
    if (fd >= 0) close(fd);
    return 0;
}
