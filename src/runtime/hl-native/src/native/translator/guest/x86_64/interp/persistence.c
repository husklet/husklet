#include "persistence.h"

#include <string.h>

void x64_pc_put16(uint8_t **cursor, uint16_t value) {
    (*cursor)[0] = (uint8_t)value;
    (*cursor)[1] = (uint8_t)(value >> 8);
    *cursor += 2;
}

void x64_pc_put32(uint8_t **cursor, uint32_t value) {
    for (unsigned i = 0; i < 4; i++) (*cursor)[i] = (uint8_t)(value >> (8 * i));
    *cursor += 4;
}

void x64_pc_put64(uint8_t **cursor, uint64_t value) {
    for (unsigned i = 0; i < 8; i++) (*cursor)[i] = (uint8_t)(value >> (8 * i));
    *cursor += 8;
}

uint16_t x64_pc_get16(const uint8_t *cursor) {
    return (uint16_t)(cursor[0] | ((uint16_t)cursor[1] << 8));
}

uint32_t x64_pc_get32(const uint8_t *cursor) {
    uint32_t value = 0;
    for (unsigned i = 0; i < 4; i++) value |= (uint32_t)cursor[i] << (8 * i);
    return value;
}

uint64_t x64_pc_get64(const uint8_t *cursor) {
    uint64_t value = 0;
    for (unsigned i = 0; i < 8; i++) value |= (uint64_t)cursor[i] << (8 * i);
    return value;
}

int x64_pc_header_validate(const uint8_t *bytes, size_t size, uint64_t abi, uint64_t cpu_size,
                           uint64_t map_slots, const uint8_t identity[32], uint64_t entry,
                           uint64_t modes, uint64_t matches[10]) {
    uint64_t local[10] = {
        size >= X64_PC_HEADER_SIZE,
        size >= 8 && x64_pc_get64(bytes) == X64_PC_MAGIC,
        size >= 16 && x64_pc_get64(bytes + 8) == X64_PC_VERSION,
        size >= 24 && x64_pc_get64(bytes + 16) == X64_PC_ENDIAN,
        size >= 32 && x64_pc_get64(bytes + 24) == abi,
        size >= 40 && x64_pc_get64(bytes + 32) == cpu_size,
        size >= 48 && x64_pc_get64(bytes + 40) == map_slots,
        size >= 80 && memcmp(bytes + 48, identity, 32) == 0,
        size >= 88 && x64_pc_get64(bytes + 80) == entry,
        size >= 200 && x64_pc_get64(bytes + 192) == modes,
    };
    int valid = 1;
    for (unsigned i = 0; i < 10; i++) {
        if (matches != NULL) matches[i] = local[i];
        valid = valid && local[i];
    }
    return valid;
}

static int x64_pc_scaled(uint64_t count, uint64_t width, uint64_t *bytes) {
    if (count > UINT64_MAX / width) return 0;
    *bytes = count * width;
    return 1;
}

int x64_pc_layout_validate(const uint8_t *bytes, size_t size, const x64_pc_format_limits *limits,
                           x64_pc_format_layout *layout, uint64_t matches[8]) {
    *layout = (x64_pc_format_layout){
        .arena = x64_pc_get64(bytes + 88),
        .maps = x64_pc_get64(bytes + 96),
        .owners = x64_pc_get64(bytes + 104),
        .helper_relocations = x64_pc_get64(bytes + 112),
        .relocations = x64_pc_get64(bytes + 184),
        .image_lo = x64_pc_get64(bytes + 200),
        .image_hi = x64_pc_get64(bytes + 208),
        .interpreter_lo = x64_pc_get64(bytes + 216),
        .interpreter_hi = x64_pc_get64(bytes + 224),
        .libraries = x64_pc_get64(bytes + 232),
        .chains = x64_pc_get64(bytes + 240),
    };
    int scaled = x64_pc_scaled(layout->maps, X64_PC_MAP_SIZE, &layout->map_bytes) &&
                 x64_pc_scaled(layout->owners, X64_PC_OWNER_SIZE, &layout->owner_bytes) &&
                 x64_pc_scaled(layout->relocations, X64_PC_RELOC_SIZE, &layout->relocation_bytes) &&
                 x64_pc_scaled(layout->helper_relocations, X64_PC_HELPER_RELOC_SIZE,
                               &layout->helper_relocation_bytes) &&
                 x64_pc_scaled(layout->libraries, X64_PC_LIB_SIZE, &layout->library_bytes) &&
                 x64_pc_scaled(layout->chains, X64_PC_CHAIN_SIZE, &layout->chain_bytes);
    uint64_t total = X64_PC_HEADER_SIZE;
    uint64_t sections[] = {layout->map_bytes, layout->owner_bytes, layout->relocation_bytes,
                           layout->helper_relocation_bytes, layout->library_bytes,
                           layout->chain_bytes, layout->arena};
    int sized = scaled;
    for (unsigned i = 0; sized && i < sizeof sections / sizeof sections[0]; i++) {
        sized = sections[i] <= UINT64_MAX - total;
        if (sized) total += sections[i];
    }
    uint64_t local[8] = {
        layout->arena != 0 && layout->arena <= limits->arena_bytes,
        layout->maps != 0 && layout->maps <= limits->maps,
        layout->owners <= limits->owners,
        layout->helper_relocations <= limits->helper_relocations,
        layout->relocations <= limits->relocations,
        layout->libraries <= limits->libraries,
        layout->chains <= limits->chains,
        sized && total == size,
    };
    int valid = scaled;
    for (unsigned i = 0; i < 8; i++) {
        if (matches != NULL) matches[i] = local[i];
        valid = valid && local[i];
    }
    valid = valid && layout->image_lo < layout->image_hi &&
            layout->interpreter_lo < layout->interpreter_hi && layout->image_hi <= layout->interpreter_lo &&
            layout->image_hi <= UINT64_C(0x0000800000000000) &&
            layout->interpreter_hi <= UINT64_C(0x0000800000000000);
    for (unsigned group = 0; valid && group < 2; group++) {
        unsigned offset = 120 + group * 32;
        uint64_t entry = x64_pc_get64(bytes + offset), rsp = x64_pc_get64(bytes + offset + 8);
        uint64_t flags = x64_pc_get64(bytes + offset + 16), end = x64_pc_get64(bytes + offset + 24);
        valid = (entry == UINT64_MAX && rsp == UINT64_MAX && flags == UINT64_MAX && end == UINT64_MAX) ||
                (entry < rsp && rsp <= flags && flags < end && end <= layout->arena);
    }
    return valid;
}
