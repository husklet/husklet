// One native run that stores into several shared views must publish a dirty
// record for each of them. Interleaving the views inside the translated loop makes a
// per-view range that is dropped, narrowed, or attributed to the wrong view
// show up as one object reading back stale.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#define VIEWS 4
#define WORDS (4u * 1024u)
#define BYTES ((size_t)WORDS * 8u)
#define ROUNDS 4u

static uint64_t mix(uint64_t view, uint64_t index, uint64_t round) {
    uint64_t value = index * 0x9e3779b97f4a7c15ull + round * 0xff51afd7ed558ccdull + view * 0x2545f4914f6cdd1dull;
    value ^= value >> 29;
    value *= 0xc2b2ae3d27d4eb4full;
    value ^= value >> 32;
    return value;
}

int main(void) {
    int fds[VIEWS];
    uint64_t *maps[VIEWS];
    for (int view = 0; view < VIEWS; view++) {
        fds[view] = (int)syscall(SYS_memfd_create, "publication", 0);
        if (fds[view] < 0 || ftruncate(fds[view], (off_t)BYTES) != 0) {
            printf("pubviews setup=0\n");
            return 1;
        }
        maps[view] = mmap(NULL, BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fds[view], 0);
        if (maps[view] == MAP_FAILED) {
            printf("pubviews mmap=0\n");
            return 1;
        }
    }
    for (uint64_t round = 0; round < ROUNDS; round++) {
        for (size_t index = 0; index < WORDS; index++) {
            for (int view = 0; view < VIEWS; view++) {
                maps[view][index] = mix((uint64_t)view, index, round);
            }
        }
    }
    static uint64_t chunk[4096];
    int ok[VIEWS];
    for (int view = 0; view < VIEWS; view++) {
        ok[view] = 1;
        size_t done = 0;
        while (done < BYTES && ok[view]) {
            size_t want = BYTES - done < sizeof chunk ? BYTES - done : sizeof chunk;
            ssize_t got = pread(fds[view], chunk, want, (off_t)done);
            if (got <= 0) {
                ok[view] = 0;
                break;
            }
            for (size_t i = 0; i < (size_t)got / 8; i++) {
                if (chunk[i] != mix((uint64_t)view, done / 8 + i, ROUNDS - 1)) {
                    ok[view] = 0;
                    break;
                }
            }
            done += (size_t)got;
        }
        munmap(maps[view], BYTES);
        close(fds[view]);
    }
    printf("pubviews");
    for (int view = 0; view < VIEWS; view++) {
        printf(" v%d=%d", view, ok[view]);
    }
    printf("\n");
    return 0;
}
