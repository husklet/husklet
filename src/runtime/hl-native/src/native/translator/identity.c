#include "identity.h"

#include "../host/cpu.h"

#include <stddef.h>
#include <string.h>

/* cpu.h duplicates the hl_host_isa numbering as literal macros because callers must name the host ISA at
 * PREPROCESSOR time, from emitters that cannot include this header. Drift is not a compile error but a
 * cache-identity hash that stops distinguishing hosts -- one host executing another's machine code. */
_Static_assert(HL_HOST_CPU_ISA_AARCH64 == HL_HOST_ISA_AARCH64, "cpu.h and hl_host_isa disagree on aarch64");
_Static_assert(HL_HOST_CPU_ISA_X86_64 == HL_HOST_ISA_X86_64, "cpu.h and hl_host_isa disagree on x86_64");

#define HL_IDENTITY_SEED 1469598103934665603ull
#define HL_IDENTITY_PRIME 1099511628211ull

typedef struct sha256_state {
    uint32_t h[8];
    uint64_t size;
    uint8_t block[64];
    size_t used;
} sha256_state;

static uint32_t sha_rotr(uint32_t value, unsigned amount) {
    return (value >> amount) | (value << (32u - amount));
}

static void sha256_compress(sha256_state *state, const uint8_t block[64]) {
    static const uint32_t constants[64] = {
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    };
    uint32_t words[64];
    for (size_t i = 0; i < 16; ++i)
        words[i] = ((uint32_t)block[i * 4] << 24) | ((uint32_t)block[i * 4 + 1] << 16) |
                   ((uint32_t)block[i * 4 + 2] << 8) | block[i * 4 + 3];
    for (size_t i = 16; i < 64; ++i) {
        uint32_t s0 = sha_rotr(words[i - 15], 7) ^ sha_rotr(words[i - 15], 18) ^ (words[i - 15] >> 3);
        uint32_t s1 = sha_rotr(words[i - 2], 17) ^ sha_rotr(words[i - 2], 19) ^ (words[i - 2] >> 10);
        words[i] = words[i - 16] + s0 + words[i - 7] + s1;
    }
    uint32_t a = state->h[0], b = state->h[1], c = state->h[2], d = state->h[3];
    uint32_t e = state->h[4], f = state->h[5], g = state->h[6], h = state->h[7];
    for (size_t i = 0; i < 64; ++i) {
        uint32_t s1 = sha_rotr(e, 6) ^ sha_rotr(e, 11) ^ sha_rotr(e, 25);
        uint32_t choice = (e & f) ^ (~e & g);
        uint32_t first = h + s1 + choice + constants[i] + words[i];
        uint32_t s0 = sha_rotr(a, 2) ^ sha_rotr(a, 13) ^ sha_rotr(a, 22);
        uint32_t majority = (a & b) ^ (a & c) ^ (b & c);
        uint32_t second = s0 + majority;
        h = g;
        g = f;
        f = e;
        e = d + first;
        d = c;
        c = b;
        b = a;
        a = first + second;
    }
    state->h[0] += a;
    state->h[1] += b;
    state->h[2] += c;
    state->h[3] += d;
    state->h[4] += e;
    state->h[5] += f;
    state->h[6] += g;
    state->h[7] += h;
}

static void sha256_init(sha256_state *state) {
    const uint32_t initial[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                                 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
    memcpy(state->h, initial, sizeof initial);
    state->size = 0;
    state->used = 0;
}

static void sha256_update(sha256_state *state, const void *bytes, size_t size) {
    const uint8_t *data = bytes;
    state->size += size;
    while (size != 0) {
        size_t take = 64 - state->used;
        if (take > size) take = size;
        memcpy(state->block + state->used, data, take);
        state->used += take;
        data += take;
        size -= take;
        if (state->used == 64) {
            sha256_compress(state, state->block);
            state->used = 0;
        }
    }
}

static hl_identity_digest sha256_finish(sha256_state *state) {
    uint64_t bits = state->size * 8;
    state->block[state->used++] = 0x80;
    if (state->used > 56) {
        memset(state->block + state->used, 0, 64 - state->used);
        sha256_compress(state, state->block);
        state->used = 0;
    }
    memset(state->block + state->used, 0, 56 - state->used);
    for (size_t i = 0; i < 8; ++i)
        state->block[63 - i] = (uint8_t)(bits >> (i * 8));
    sha256_compress(state, state->block);
    hl_identity_digest result;
    for (size_t i = 0; i < 8; ++i)
        for (size_t j = 0; j < 4; ++j)
            result.bytes[i * 4 + j] = (uint8_t)(state->h[i] >> (24 - j * 8));
    return result;
}

static uint64_t identity_bytes(uint64_t value, const char *bytes) {
    while (*bytes != '\0') {
        value ^= (uint8_t)*bytes++;
        value *= HL_IDENTITY_PRIME;
    }
    return value;
}

uint64_t hl_identity_name(const char *name) {
    const char *base;
    const char *cursor;

    if (name == NULL) return 0x1357ull;
    base = name;
    for (cursor = name; *cursor != '\0'; ++cursor)
        if (*cursor == '/') base = cursor + 1;
    return identity_bytes(HL_IDENTITY_SEED, base);
}

uint64_t hl_identity_image(const void *bytes, size_t size) {
    const uint8_t *data = bytes;
    uint64_t value = HL_IDENTITY_SEED;
    for (size_t index = 0; index < sizeof size; ++index) {
        value ^= (uint8_t)(size >> (index * 8u));
        value *= HL_IDENTITY_PRIME;
    }
    for (size_t index = 0; index < size; ++index) {
        value ^= data[index];
        value *= HL_IDENTITY_PRIME;
    }
    return value;
}

hl_identity_digest hl_identity_image_digest(const void *bytes, size_t size) {
    sha256_state state;
    sha256_init(&state);
    sha256_update(&state, bytes, size);
    return sha256_finish(&state);
}

hl_identity_digest hl_identity_digest_mix(hl_identity_digest program, hl_identity_digest interpreter, uint64_t engine,
                                          uint64_t name) {
    static const uint8_t domain[] = "husklet-pcache-executable-v1";
    sha256_state state;
    sha256_init(&state);
    sha256_update(&state, domain, sizeof domain);
    sha256_update(&state, program.bytes, sizeof program.bytes);
    sha256_update(&state, interpreter.bytes, sizeof interpreter.bytes);
    sha256_update(&state, &engine, sizeof engine);
    sha256_update(&state, &name, sizeof name);
    return sha256_finish(&state);
}

int hl_identity_digest_equal(const hl_identity_digest *left, const hl_identity_digest *right) {
    uint8_t difference = 0;
    for (size_t i = 0; i < sizeof left->bytes; ++i)
        difference |= left->bytes[i] ^ right->bytes[i];
    return difference == 0;
}

int hl_identity_digest_empty(const hl_identity_digest *digest) {
    uint8_t any = 0;
    for (size_t i = 0; i < sizeof digest->bytes; ++i)
        any |= digest->bytes[i];
    return any == 0;
}

uint64_t hl_identity_file(const hl_host_file_metadata *metadata) {
    uint64_t value = HL_IDENTITY_SEED;
    uint64_t fields[5];
    size_t index;
    if (metadata == NULL || metadata->type != HL_HOST_FILE_TYPE_REGULAR) return 0;
    fields[0] = metadata->stable_device;
    fields[1] = metadata->stable_object;
    fields[2] = metadata->size;
    fields[3] = metadata->modified_ns / UINT64_C(1000000000);
    fields[4] = metadata->modified_ns % UINT64_C(1000000000);
    for (index = 0; index < sizeof(fields) / sizeof(fields[0]); ++index) {
        value ^= fields[index];
        value *= HL_IDENTITY_PRIME;
    }
    return value;
}

uint64_t hl_identity_source(const hl_host_services *services, const char *path) {
    hl_host_file_metadata metadata;
    hl_host_result opened, result;
    uint64_t value;
    if (services == NULL || services->file == NULL || services->file->abi != HL_HOST_FILE_ABI ||
        services->file->size < sizeof(*services->file) || services->file->open_relative == NULL ||
        services->file->metadata == NULL || services->file->close == NULL || path == NULL || path[0] == 0)
        return 0;
    opened = services->file->open_relative(services->context, HL_HOST_HANDLE_CWD, path, strlen(path),
                                           HL_HOST_FILE_READ | HL_HOST_FILE_NOFOLLOW, 0, 0);
    if (opened.status != HL_STATUS_OK) return 0;
    result = services->file->metadata(services->context, opened.value, &metadata);
    if (services->file->close(services->context, opened.value).status != HL_STATUS_OK ||
        result.status != HL_STATUS_OK || metadata.type != HL_HOST_FILE_TYPE_REGULAR)
        return 0;
    value = hl_identity_file(&metadata);
    value ^= metadata.changed_ns;
    value *= HL_IDENTITY_PRIME;
    return identity_bytes(value, path);
}

uint64_t hl_identity_mix(uint64_t program, uint64_t interpreter, uint64_t engine, uint64_t name) {
    return (program ^ (interpreter * HL_IDENTITY_PRIME)) ^ engine ^ (name * HL_IDENTITY_PRIME);
}

uint64_t hl_identity_configuration(uint64_t build, uint32_t guest_isa, uint32_t host_isa, uint64_t modes) {
    uint64_t value = HL_IDENTITY_SEED;
    const uint64_t fields[] = {build, guest_isa, host_isa, modes};
    size_t index;
    for (index = 0; index < sizeof(fields) / sizeof(fields[0]); ++index) {
        value ^= fields[index];
        value *= HL_IDENTITY_PRIME;
    }
    return value;
}
