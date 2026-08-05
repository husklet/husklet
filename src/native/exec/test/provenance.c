#include "support.h"

#include "../cache/cache.h"
#include "../src/arena.h"

#include <stdio.h>
#include <string.h>

#define CHECK(expression)                                                                                              \
    do {                                                                                                               \
        if (!(expression)) {                                                                                           \
            fprintf(stderr, "provenance:%d: %s\n", __LINE__, #expression);                                          \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

static int reconstruction(void) {
    uint64_t registers[32] = {0};
    uint64_t address = 0;
    hl_native_address descriptor = {.kind = HL_NATIVE_ADDRESS_BASE, .bits = 64,
                                    .base = 5, .displacement = -8};
    registers[5] = 0x1008;
    CHECK(hl_native_address_reconstruct(&descriptor, registers, 32, &address) && address == 0x1000);
    descriptor = (hl_native_address){.kind = HL_NATIVE_ADDRESS_INDEXED, .bits = 32,
                                     .base = 5, .index = 6, .shift = 3,
                                     .extend = HL_NATIVE_EXTEND_U32, .displacement = 4};
    registers[5] = UINT64_C(0xffffffff00000008);
    registers[6] = UINT64_C(0xffffffff00000002);
    CHECK(hl_native_address_reconstruct(&descriptor, registers, 32, &address) && address == 28);
    descriptor = (hl_native_address){.kind = HL_NATIVE_ADDRESS_CONSTANT, .bits = 32,
                                     .constant = UINT64_C(0xffffffff12345678)};
    CHECK(hl_native_address_reconstruct(&descriptor, NULL, 0, &address) && address == 0x12345678);
    descriptor.kind = HL_NATIVE_ADDRESS_NONE;
    CHECK(!hl_native_address_reconstruct(&descriptor, registers, 32, &address));
    descriptor = (hl_native_address){.kind = HL_NATIVE_ADDRESS_BASE, .bits = 64, .base = 32};
    CHECK(!hl_native_address_reconstruct(&descriptor, registers, 32, &address));
    return 0;
}

int main(void) {
    test_memory host = {0};
    hl_native_memory services = test_services(&host);
    hl_native_config config = test_config(&services, 0);
    hl_native_arena arena;
    hl_native_cache *cache = NULL;
    hl_native_block block;
    hl_native_code code;
    hl_native_provenance records[2] = {
        {.code_offset = 0, .code_size = 4, .guest = 0x4000},
        {.code_offset = 3, .code_size = 4, .guest = 0x4004},
    };
    uint64_t guest = 0;
    uint64_t address = 0;
    uint64_t registers[32] = {0};
    hl_native_provenance resolved = {0};

    CHECK(reconstruction() == 0);
    CHECK(hl_native_arena_create(&arena, &config) == HL_NATIVE_OK);
    CHECK(hl_native_cache_create(&cache, &arena, 8, 2, 2, 1, NULL) == HL_NATIVE_OK);
    CHECK(hl_native_arena_begin(&arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_reserve(cache, 0x4000, 1, 0x4000, 0x4008, 16, &block) == HL_NATIVE_OK);
    memset(arena.writable + block.code_offset, 0xd5, 8);
    CHECK(hl_native_cache_publish_map(cache, &block, 8, 0, records, 2) == HL_NATIVE_ARGUMENT);
    CHECK(!hl_native_cache_provenance(cache, arena.executable, &guest));

    records[1].code_offset = 4;
    records[0].access = HL_NATIVE_ACCESS_READ;
    records[0].width = 8;
    records[0].address = (hl_native_address){.kind = HL_NATIVE_ADDRESS_CONSTANT, .bits = 64,
                                             .constant = 0x12345678};
    records[1].access = HL_NATIVE_ACCESS_WRITE;
    records[1].width = 4;
    records[1].address = (hl_native_address){.kind = HL_NATIVE_ADDRESS_INDEXED, .bits = 64,
                                             .base = 3, .index = 7, .shift = 2,
                                             .extend = HL_NATIVE_EXTEND_S32, .displacement = -16};
    CHECK(hl_native_cache_publish_map(cache, &block, 8, 0, records, 2) == HL_NATIVE_OK);
    CHECK(hl_native_arena_end(&arena) == HL_NATIVE_OK);
    CHECK(hl_native_cache_provenance(cache, arena.executable + 6, &guest) && guest == 0x4004);
    CHECK(hl_native_cache_provenance_record(cache, arena.executable + 2, &resolved));
    CHECK(resolved.code_size == 4 && resolved.guest == 0x4000 &&
          resolved.access == HL_NATIVE_ACCESS_READ && resolved.width == 8);
    CHECK(hl_native_address_reconstruct(&resolved.address, NULL, 0, &address) && address == 0x12345678);
    CHECK(hl_native_cache_provenance_record(cache, arena.executable + 6, &resolved));
    registers[3] = 0x2000;
    registers[7] = UINT64_C(0xffffffff);
    CHECK(hl_native_address_reconstruct(&resolved.address, registers, 32, &address));
    CHECK(address == 0x1fec);
    CHECK(!hl_native_address_reconstruct(&resolved.address, registers, 4, &address));
    CHECK(hl_native_cache_invalidate(cache, 0x4000, 0x4008, NULL) == HL_NATIVE_OK);
    CHECK(hl_native_cache_lookup(cache, 0x4000, 1, &code) == HL_NATIVE_MISS);
    CHECK(hl_native_cache_provenance(cache, arena.executable + 6, &guest) && guest == 0x4004);
    CHECK(hl_native_cache_reset(cache, 2) == HL_NATIVE_OK);
    CHECK(!hl_native_cache_provenance(cache, arena.executable + 2, &guest));

    hl_native_cache_destroy(cache);
    hl_native_arena_destroy(&arena);
    CHECK(host.release_calls == 1);
    return 0;
}
