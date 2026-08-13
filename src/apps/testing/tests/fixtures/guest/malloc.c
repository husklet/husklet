#include <errno.h>
#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>
#include <unistd.h>

#if defined(HL_SQLITE_LAYOUT)
#include <sqlite3.h>
#define HL_LAYOUT "sqlite"
#else
#define HL_LAYOUT "plain"
#endif

static uint64_t micros(void) {
    struct timespec value;
    if (clock_gettime(CLOCK_MONOTONIC, &value) != 0) {
        abort();
    }
    return (uint64_t)value.tv_sec * UINT64_C(1000000) + (uint64_t)value.tv_nsec / UINT64_C(1000);
}

static int write_all(const char *buffer, size_t length) {
    while (length != 0) {
        ssize_t written = write(STDOUT_FILENO, buffer, length);
        if (written < 0 && errno == EINTR) {
            continue;
        }
        if (written <= 0) {
            return -1;
        }
        buffer += (size_t)written;
        length -= (size_t)written;
    }
    return 0;
}

int main(void) {
#if defined(HL_SQLITE_LAYOUT)
    if (sqlite3_initialize() != SQLITE_OK) {
        return 3;
    }
#endif
    uint64_t compute_proof = 0;
    uint64_t started = micros();
    for (uint64_t index = 0; index < UINT64_C(128000000); ++index) {
        compute_proof = compute_proof * UINT64_C(6364136223846793005) + index + UINT64_C(1442695040888963407);
    }
    uint64_t compute = micros() - started;

    uint64_t malloc_proof = 0;
    started = micros();
    for (size_t sweep = 0; sweep < 4096; ++sweep) {
        for (size_t slot = 0; slot < 241; ++slot) {
            size_t size = 1 + ((slot * 17 + sweep * 131) % 4096);
            volatile unsigned char *allocation = malloc(size);
            if (allocation == NULL) {
                return 2;
            }
            unsigned char first = (unsigned char)(size ^ sweep);
            unsigned char last = (unsigned char)((size >> 8) ^ slot);
            allocation[0] = first;
            allocation[size - 1] = last;
            malloc_proof = malloc_proof * UINT64_C(1099511628211) + size;
            malloc_proof ^= (uint64_t)allocation[0] | (uint64_t)allocation[size - 1] << 8;
            free((void *)allocation);
        }
    }
    uint64_t malloc_time = micros() - started;

    char frame[256];
    int length = snprintf(frame, sizeof(frame),
                          "META workload=malloc layout=%s version=1\n"
                          "PHASE compute us=%" PRIu64 " ok=%" PRIu64 "\n"
                          "PHASE malloc us=%" PRIu64 " ok=%" PRIu64 "\n",
                          HL_LAYOUT, compute ? compute : 1, compute_proof, malloc_time ? malloc_time : 1, malloc_proof);
    if (length < 0 || (size_t)length >= sizeof(frame) || write_all(frame, (size_t)length) != 0) {
        return 4;
    }
#if defined(HL_SQLITE_LAYOUT)
    sqlite3_shutdown();
#endif
    return 0;
}
