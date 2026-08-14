#include <errno.h>
#include <inttypes.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
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

struct work_factor {
    const char *text;
    uint64_t factor;
    uint64_t compute_proof;
    uint64_t malloc_proof;
};

_Static_assert(UINT64_MAX / UINT64_C(128) >= UINT64_C(128000000), "compute factor multiplication overflows");
_Static_assert(SIZE_MAX / (size_t)128 >= (size_t)4096, "malloc factor multiplication overflows");

/* Fixed proof constants make each allowed factor validate independently of its loop bounds. */
static const struct work_factor FACTORS[] = {
    {"1", UINT64_C(1), UINT64_C(9686655140321103872), UINT64_C(10725705084448409897)},
    {"2", UINT64_C(2), UINT64_C(17644014193459470336), UINT64_C(6528316421022001472)},
    {"4", UINT64_C(4), UINT64_C(5739948047395471360), UINT64_C(1674606823239620224)},
    {"8", UINT64_C(8), UINT64_C(13729380265047392256), UINT64_C(1493020912505358080)},
    {"16", UINT64_C(16), UINT64_C(13090333494462185472), UINT64_C(336581374273486336)},
    {"32", UINT64_C(32), UINT64_C(3136977997641416704), UINT64_C(16331624503495730176)},
    {"64", UINT64_C(64), UINT64_C(5071912503035035648), UINT64_C(5324999304749099008)},
    {"128", UINT64_C(128), UINT64_C(13694331945478520832), UINT64_C(17599653024954822656)},
};

static const struct work_factor *parse_factor(const char *text, size_t length) {
    for (size_t index = 0; index < sizeof(FACTORS) / sizeof(FACTORS[0]); ++index) {
        if (strlen(FACTORS[index].text) == length && memcmp(FACTORS[index].text, text, length) == 0) {
            return &FACTORS[index];
        }
    }
    return NULL;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        return 1;
    }
    const char *separator = strchr(argv[1], ',');
    if (separator == NULL || strchr(separator + 1, ',') != NULL) {
        return 1;
    }
    const struct work_factor *compute_factor = parse_factor(argv[1], (size_t)(separator - argv[1]));
    const struct work_factor *malloc_factor = parse_factor(separator + 1, strlen(separator + 1));
    if (compute_factor == NULL || malloc_factor == NULL) {
        return 1;
    }
#if defined(HL_SQLITE_LAYOUT)
    if (sqlite3_initialize() != SQLITE_OK) {
        return 3;
    }
#endif
    uint64_t compute_proof = 0;
    uint64_t started = micros();
    for (uint64_t index = 0; index < UINT64_C(128000000) * compute_factor->factor; ++index) {
        compute_proof = compute_proof * UINT64_C(6364136223846793005) + index + UINT64_C(1442695040888963407);
    }
    uint64_t compute = micros() - started;

    uint64_t malloc_proof = 0;
    started = micros();
    for (size_t sweep = 0; sweep < (size_t)4096 * (size_t)malloc_factor->factor; ++sweep) {
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
    if (compute_proof != compute_factor->compute_proof || malloc_proof != malloc_factor->malloc_proof) {
        return 5;
    }

    char frame[256];
    int length = snprintf(frame, sizeof(frame),
                          "META workload=malloc layout=%s version=1 factor=%s,%s\n"
                          "PHASE compute us=%" PRIu64 " ok=%" PRIu64 "\n"
                          "PHASE malloc us=%" PRIu64 " ok=%" PRIu64 "\n",
                          HL_LAYOUT, compute_factor->text, malloc_factor->text, compute ? compute : 1, compute_proof,
                          malloc_time ? malloc_time : 1, malloc_proof);
    if (length < 0 || (size_t)length >= sizeof(frame) || write_all(frame, (size_t)length) != 0) {
        return 4;
    }
#if defined(HL_SQLITE_LAYOUT)
    sqlite3_shutdown();
#endif
    return 0;
}
