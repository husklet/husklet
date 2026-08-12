#ifndef HL_TRANSLATOR_DIGEST_H
#define HL_TRANSLATOR_DIGEST_H

#include <stddef.h>
#include <stdint.h>

#define HL_DIGEST_SEED UINT64_C(1469598103934665603)

typedef struct hl_digest {
    uint64_t value;
} hl_digest;

void hl_digest_init(hl_digest *digest, uint64_t seed);
void hl_digest_update(hl_digest *digest, const void *bytes, size_t size);
uint64_t hl_digest_value(const hl_digest *digest);
uint64_t hl_digest_bytes(uint64_t seed, const void *bytes, size_t size);

#endif
