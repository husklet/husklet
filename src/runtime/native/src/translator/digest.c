#include "digest.h"

#include <string.h>

void hl_digest_init(hl_digest *digest, uint64_t seed) {
    digest->value = seed;
}

void hl_digest_update(hl_digest *digest, const void *bytes, size_t size) {
    const uint8_t *cursor = bytes;
    uint64_t value = digest->value;
    /* Preserve the production cache's word-at-a-time checksum byte-for-byte. */
    for (; size >= 8; cursor += 8, size -= 8) {
        uint64_t word;
        memcpy(&word, cursor, sizeof(word));
        value ^= word;
        value *= UINT64_C(1099511628211);
    }
    for (; size != 0; ++cursor, --size) {
        value ^= *cursor;
        value *= UINT64_C(1099511628211);
    }
    digest->value = value;
}

uint64_t hl_digest_value(const hl_digest *digest) {
    return digest->value;
}

uint64_t hl_digest_bytes(uint64_t seed, const void *bytes, size_t size) {
    hl_digest digest;
    hl_digest_init(&digest, seed);
    hl_digest_update(&digest, bytes, size);
    return hl_digest_value(&digest);
}
