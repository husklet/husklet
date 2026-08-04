// MAP_FIXED_NOREPLACE on a NON-bound (plain anonymous) mapping must reproduce Linux's collision semantics
// itself: mapping over an existing region fails with EEXIST and leaves that region's bytes untouched (a
// clobber here is silent data corruption for allocators that use NOREPLACE as a safe-placement probe),
// while mapping into a genuinely free hole places at the exact address and zero-fills. Plain MAP_FIXED, by
// contrast, atomically REPLACES the existing region -- the difference is the whole point of the flag.
#define _GNU_SOURCE
#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef MAP_FIXED_NOREPLACE
#define MAP_FIXED_NOREPLACE 0x100000
#endif

int main(void) {
    long ps = sysconf(_SC_PAGESIZE);
    size_t len = (size_t)ps;

    unsigned char *occupied = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (occupied == MAP_FAILED) return 1;
    memset(occupied, 0xa7, len);

    // NOREPLACE over the existing mapping -> EEXIST, canary intact.
    errno = 0;
    void *collide = mmap(occupied, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE,
                         -1, 0);
    int rejected = collide == MAP_FAILED && errno == EEXIST;
    int preserved = occupied[0] == 0xa7 && occupied[len - 1] == 0xa7;

    // A free hole (carved from a reservation) -> placed at the exact address and zero-filled.
    unsigned char *arena = mmap(NULL, len * 3, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (arena == MAP_FAILED) return 1;
    unsigned char *hole = arena + len;
    if (munmap(hole, len) != 0) return 1;
    errno = 0;
    unsigned char *placed = mmap(hole, len, PROT_READ | PROT_WRITE,
                                 MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    int at_addr = placed == hole;
    int zeroed = at_addr && placed[0] == 0 && placed[len - 1] == 0;

    // Plain MAP_FIXED over the existing mapping REPLACES it (contrast with NOREPLACE above).
    void *repl = mmap(occupied, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0);
    int replaced = repl == occupied && occupied[0] == 0;

    printf("noreplace_anon rejected=%d preserved=%d placed=%d zeroed=%d fixed_replaced=%d\n", rejected,
           preserved, at_addr, zeroed, replaced);
    if (at_addr) munmap(placed, len);
    munmap(arena, len * 3);
    munmap(occupied, len);
    return rejected && preserved && at_addr && zeroed && replaced ? 0 : 1;
}
