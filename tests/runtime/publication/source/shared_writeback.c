// Native stores into a MAP_SHARED file mapping are only visible to the rest of
// the system once the engine publishes them, so this reads the pattern back
// through pread and a second independent mapping rather than the writing view.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

#define WORDS (8u * 1024u)
#define BYTES ((size_t)WORDS * 8u)
#define ROUNDS 4u

static uint64_t mix(uint64_t index, uint64_t round) {
    uint64_t value = index * 0x9e3779b97f4a7c15ull + round * 0xff51afd7ed558ccdull;
    value ^= value >> 29;
    value *= 0xc2b2ae3d27d4eb4full;
    value ^= value >> 32;
    return value;
}

// Reads the file through the descriptor, a path that never observes the mapped view.
static int pread_matches(int fd, uint64_t edge_low, uint64_t edge_high) {
    static uint64_t chunk[8192];
    size_t done = 0;
    while (done < BYTES) {
        size_t want = BYTES - done < sizeof chunk ? BYTES - done : sizeof chunk;
        ssize_t got = pread(fd, chunk, want, (off_t)done);
        if (got <= 0) {
            return 0;
        }
        for (size_t i = 0; i < (size_t)got / 8; i++) {
            size_t index = done / 8 + i;
            uint64_t want_value = mix(index, ROUNDS - 1);
            if (index == 0) {
                want_value = edge_low;
            }
            if (index == WORDS - 1) {
                want_value = edge_high;
            }
            if (chunk[i] != want_value) {
                return 0;
            }
        }
        done += (size_t)got;
    }
    return 1;
}

static int alias_matches(int fd, uint64_t edge_low, uint64_t edge_high) {
    uint64_t *alias = mmap(NULL, BYTES, PROT_READ, MAP_SHARED, fd, 0);
    if (alias == MAP_FAILED) {
        return 0;
    }
    int ok = 1;
    for (size_t index = 0; index < WORDS; index++) {
        uint64_t want_value = mix(index, ROUNDS - 1);
        if (index == 0) {
            want_value = edge_low;
        }
        if (index == WORDS - 1) {
            want_value = edge_high;
        }
        if (alias[index] != want_value) {
            ok = 0;
            break;
        }
    }
    munmap(alias, BYTES);
    return ok;
}

int main(void) {
    int fd = open("/tmp/publication-writeback", O_RDWR | O_CREAT | O_TRUNC, 0600);
    if (fd < 0 || ftruncate(fd, (off_t)BYTES) != 0) {
        printf("pubshared open=0\n");
        return 1;
    }
    uint64_t *map = mmap(NULL, BYTES, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (map == MAP_FAILED) {
        printf("pubshared mmap=0\n");
        return 1;
    }
    // Repeated enough to translate the store loop without turning a bounded
    // compatibility assertion into a multi-million-call observer soak.
    for (uint64_t round = 0; round < ROUNDS; round++) {
        for (size_t index = 0; index < WORDS; index++) {
            map[index] = mix(index, round);
        }
    }
    // The extreme words carry distinct values so a dirty range that is narrowed
    // at either end loses exactly one of them.
    uint64_t edge_low = 0x1122334455667788ull;
    uint64_t edge_high = 0x99aabbccddeeff00ull;
    map[0] = edge_low;
    map[WORDS - 1] = edge_high;
    msync(map, BYTES, MS_SYNC);

    int by_pread = pread_matches(fd, edge_low, edge_high);
    int by_alias = alias_matches(fd, edge_low, edge_high);
    munmap(map, BYTES);
    close(fd);
    printf("pubshared pread=%d alias=%d\n", by_pread, by_alias);
    return 0;
}
