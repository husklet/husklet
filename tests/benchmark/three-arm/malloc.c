#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <time.h>

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

int main(void) {
#if defined(HL_SQLITE_LAYOUT)
    if (sqlite3_initialize() != SQLITE_OK) {
        return 3;
    }
#endif
    uint64_t checksum = 0;
    uint64_t started = micros();
    for (uint64_t index = 0; index < UINT64_C(200000); ++index) {
        checksum = checksum * UINT64_C(6364136223846793005) + index + UINT64_C(1442695040888963407);
    }
    uint64_t compute = micros() - started;

    started = micros();
    for (size_t size = 1; size <= 4096; size += 17) {
        unsigned char *allocation = malloc(size);
        if (allocation == NULL) {
            return 2;
        }
        allocation[0] = (unsigned char)size;
        allocation[size - 1] = (unsigned char)(size >> 8);
        checksum += allocation[0];
        checksum += allocation[size - 1];
        free(allocation);
    }
    uint64_t malloc_time = micros() - started;

    printf("META workload=malloc layout=%s version=1\n", HL_LAYOUT);
    printf("PHASE compute us=%" PRIu64 " ok=%" PRIu64 "\n", compute ? compute : 1, checksum);
    printf("PHASE malloc us=%" PRIu64 " ok=%" PRIu64 "\n", malloc_time ? malloc_time : 1, checksum);
#if defined(HL_SQLITE_LAYOUT)
    sqlite3_shutdown();
#endif
    return 0;
}
