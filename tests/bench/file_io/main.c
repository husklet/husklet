#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/uio.h>
#include <time.h>
#include <unistd.h>

enum { BLOCK = 4096, SCALAR_ITERATIONS = 5000, VECTOR_ITERATIONS = 1000, PAGES = 256, MAPPING_ROUNDS = 300 };

static uint64_t now_us(void) {
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0) return 0;
    return (uint64_t)value.tv_sec * UINT64_C(1000000) + (uint64_t)value.tv_nsec / 1000;
}

static int temporary(char path[], size_t size) {
    int descriptor = mkstemp(path);
    if (descriptor < 0) return -1;
    (void)unlink(path);
    if (ftruncate(descriptor, (off_t)size) != 0) {
        (void)close(descriptor);
        return -1;
    }
    return descriptor;
}

static int scalar_phase(void) {
    char path[] = "hl-file-io-XXXXXX";
    unsigned char block[BLOCK];
    int descriptor = temporary(path, BLOCK * PAGES);
    if (descriptor < 0) return 1;
    memset(block, 0x5a, sizeof(block));
    uint64_t checksum = 0;
    uint64_t started = now_us();
    for (unsigned index = 0; index < SCALAR_ITERATIONS; ++index) {
        off_t offset = (off_t)(index % PAGES) * BLOCK;
        if (pwrite(descriptor, block, sizeof(block), offset) != BLOCK ||
            pread(descriptor, block, sizeof(block), offset) != BLOCK) return 1;
        checksum += block[index % BLOCK];
    }
    uint64_t elapsed = now_us() - started;
    if (close(descriptor) != 0) return 1;
    printf("PHASE scalar_file us=%llu ok=%llu\n", (unsigned long long)elapsed,
           (unsigned long long)checksum);
    return 0;
}

static int vector_phase(void) {
    char path[] = "hl-vector-io-XXXXXX";
    unsigned char first[1024], second[3072];
    struct iovec vectors[2] = {{first, sizeof(first)}, {second, sizeof(second)}};
    int descriptor = temporary(path, BLOCK);
    if (descriptor < 0) return 1;
    memset(first, 0x31, sizeof(first));
    memset(second, 0x62, sizeof(second));
    uint64_t checksum = 0;
    uint64_t started = now_us();
    for (unsigned index = 0; index < VECTOR_ITERATIONS; ++index) {
        if (lseek(descriptor, 0, SEEK_SET) != 0 || writev(descriptor, vectors, 2) != BLOCK ||
            lseek(descriptor, 0, SEEK_SET) != 0 || readv(descriptor, vectors, 2) != BLOCK) return 1;
        checksum += first[index % sizeof(first)] + second[index % sizeof(second)];
    }
    uint64_t elapsed = now_us() - started;
    if (close(descriptor) != 0) return 1;
    printf("PHASE vector_file us=%llu ok=%llu\n", (unsigned long long)elapsed,
           (unsigned long long)checksum);
    return 0;
}

static int mapping_phase(void) {
    char path[] = "hl-mapped-io-XXXXXX";
    size_t size = BLOCK * PAGES;
    int descriptor = temporary(path, size);
    if (descriptor < 0) return 1;
    unsigned char *mapping = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, descriptor, 0);
    if (mapping == MAP_FAILED) return 1;
    uint64_t checksum = 0;
    uint64_t started = now_us();
    for (unsigned round = 0; round < MAPPING_ROUNDS; ++round) {
        for (unsigned page = 0; page < PAGES; ++page) {
            size_t offset = (size_t)page * BLOCK;
            mapping[offset] = (unsigned char)(round + page);
            checksum += mapping[offset];
        }
    }
    uint64_t elapsed = now_us() - started;
    if (msync(mapping, size, MS_SYNC) != 0 || munmap(mapping, size) != 0 || close(descriptor) != 0) return 1;
    printf("PHASE mapped_file us=%llu ok=%llu\n", (unsigned long long)elapsed,
           (unsigned long long)checksum);
    return 0;
}

int main(int argc, char **argv) {
    if (argc != 3 || strcmp(argv[1], "--phase") != 0) return 2;
    if (strcmp(argv[2], "scalar") == 0) return scalar_phase();
    if (strcmp(argv[2], "vector") == 0) return vector_phase();
    if (strcmp(argv[2], "mapping") == 0) return mapping_phase();
    return 2;
}
