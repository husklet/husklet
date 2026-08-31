#include "persistence.h"
#include "../../../digest.h"

#include <stdlib.h>
#include <stdio.h>
#include <string.h>

static hl_persist_directory x64_pc_directory;
static char x64_pc_directory_path[1024];

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

const uint8_t *x64_pc_map_for_offset(uint64_t offset, const uint8_t *records, uint64_t maps, uint64_t arena) {
    uint64_t lo = 0, hi = maps;
    while (lo < hi) {
        uint64_t mid = lo + (hi - lo) / 2;
#ifdef HL_PCACHE_OFFSET_BOUNDARY_MUTATION
        if (x64_pc_get64(records + mid * X64_PC_MAP_SIZE + 24) < offset)
#else
        if (x64_pc_get64(records + mid * X64_PC_MAP_SIZE + 24) <= offset)
#endif
            lo = mid + 1;
        else
            hi = mid;
    }
    if (lo == 0) return NULL;
    uint64_t ordinal = lo - 1;
    uint64_t end = ordinal + 1 < maps
        ? x64_pc_get64(records + (ordinal + 1) * X64_PC_MAP_SIZE + 24) : arena;
    return offset < end ? records + ordinal * X64_PC_MAP_SIZE : NULL;
}

static int x64_pc_gpc_index_compare(const void *left, const void *right) {
    const x64_pc_gpc_index_entry *a = left, *b = right;
    return a->gpc < b->gpc ? -1 : a->gpc > b->gpc;
}

void x64_pc_gpc_index_build(const uint8_t *records, uint64_t maps, x64_pc_gpc_index_entry *index) {
    for (uint64_t i = 0; i < maps; i++)
        index[i] = (x64_pc_gpc_index_entry){x64_pc_get64(records + i * X64_PC_MAP_SIZE), i};
    qsort(index, (size_t)maps, sizeof *index, x64_pc_gpc_index_compare);
}

const uint8_t *x64_pc_gpc_index_find(uint64_t gpc, const uint8_t *records,
                                     const x64_pc_gpc_index_entry *index, uint64_t maps) {
    uint64_t lo = 0, hi = maps;
    while (lo < hi) {
        uint64_t mid = lo + (hi - lo) / 2;
#if defined(HL_PCACHE_GPC_BOUNDARY_MUTATION)
        if (index[mid].gpc <= gpc) lo = mid + 1;
#else
        if (index[mid].gpc < gpc) lo = mid + 1;
#endif
        else hi = mid;
    }
    return lo < maps && index[lo].gpc == gpc
        ? records + index[lo].ordinal * X64_PC_MAP_SIZE : NULL;
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
    if (valid) {
        layout->map_records = bytes + X64_PC_HEADER_SIZE;
        layout->owner_records = layout->map_records + layout->map_bytes;
        layout->relocation_records = layout->owner_records + layout->owner_bytes;
        layout->helper_relocation_records = layout->relocation_records + layout->relocation_bytes;
        layout->library_records = layout->helper_relocation_records + layout->helper_relocation_bytes;
        layout->chain_records = layout->library_records + layout->library_bytes;
        layout->arena_bytes = layout->chain_records + layout->chain_bytes;
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

static uint64_t x64_pc_checksum(const uint8_t *bytes, size_t size) {
    static const uint8_t zero[8];
    hl_digest digest;
    hl_digest_init(&digest, HL_DIGEST_SEED);
    hl_digest_update(&digest, bytes, X64_PC_CHECKSUM_OFFSET);
    hl_digest_update(&digest, zero, sizeof zero);
    hl_digest_update(&digest, bytes + X64_PC_HEADER_SIZE, size - X64_PC_HEADER_SIZE);
    return hl_digest_value(&digest);
}

int x64_pc_checksum_validate(const uint8_t *bytes, size_t size) {
    return size >= X64_PC_HEADER_SIZE && x64_pc_get64(bytes + X64_PC_CHECKSUM_OFFSET) == x64_pc_checksum(bytes, size);
}

void x64_pc_checksum_write(uint8_t *bytes, size_t size) {
    uint8_t *cursor = bytes + X64_PC_CHECKSUM_OFFSET;
    x64_pc_put64(&cursor, x64_pc_checksum(bytes, size));
}

int x64_pc_validate_maps_owners(const x64_pc_format_layout *layout,
                                const x64_pc_semantic_policy *policy, unsigned *stage) {
    uint64_t *seen_gpc = calloc(policy->map_slots, sizeof *seen_gpc);
    uint8_t *seen_used = calloc(policy->map_slots, 1);
    int valid = seen_gpc != NULL && seen_used != NULL;
    if (stage != NULL) *stage = 1;
    for (uint64_t i = 0; valid && i < layout->maps; i++) {
        const uint8_t *record = layout->map_records + i * X64_PC_MAP_SIZE;
        uint64_t host = x64_pc_get64(record + 24), body = x64_pc_get64(record + 32);
        uint64_t block = x64_pc_get64(record + 40), gpc = x64_pc_get64(record);
        uint64_t start = x64_pc_get64(record + 8), end = x64_pc_get64(record + 16);
        uint32_t entry = x64_pc_get32(record + 72), length = x64_pc_get32(record + 76);
        uint16_t ordinal = x64_pc_get16(record + 82);
        valid = host < layout->arena && body < layout->arena && host == body && block == body &&
                start <= gpc && gpc < end && x64_pc_get64(record + 48) == policy->block_magic &&
                x64_pc_get64(record + 56) == gpc && ((entry == 0) == (length == 0)) &&
                (entry == 0 || entry >= 52) && host <= layout->arena && entry <= layout->arena - host &&
                length <= layout->arena - host - entry &&
                (policy->require_census_ordinal ? ordinal < UINT16_MAX : ordinal == UINT16_MAX) &&
                layout->arena >= 52 && body <= layout->arena - 52 &&
                x64_pc_get64(layout->arena_bytes + body + 16) == x64_pc_get64(record + 64) &&
                (x64_pc_get16(layout->arena_bytes + body + 50) == 0
                     ? UINT16_MAX
                     : (uint16_t)(x64_pc_get16(layout->arena_bytes + body + 50) - 1u)) == ordinal &&
                x64_pc_get32(record + 84) <= layout->owners &&
                x64_pc_get32(record + 88) <= layout->owners - x64_pc_get32(record + 84) &&
                x64_pc_get32(record + 92) <= layout->chains &&
                x64_pc_get32(record + 96) <= layout->chains - x64_pc_get32(record + 92);
        if (valid && i != 0) {
            const uint8_t *prior = record - X64_PC_MAP_SIZE;
            uint64_t prior_host = x64_pc_get64(prior + 24);
            uint32_t prior_length = x64_pc_get32(prior + 76);
            uint32_t prior_entry = x64_pc_get32(prior + 72);
            uint64_t delta = host - prior_host;
            valid = prior_host < host && delta >= 52 && prior_entry <= delta &&
                    prior_length <= delta - prior_entry;
        }
        uint64_t slot = (gpc ^ (gpc >> 32)) % policy->map_slots, probes = 0;
        while (valid && seen_used[slot] && seen_gpc[slot] != gpc && ++probes < policy->map_slots)
            slot = (slot + 1) % policy->map_slots;
        if (valid && seen_used[slot]) valid = 0;
        if (valid) {
            seen_used[slot] = 1;
            seen_gpc[slot] = gpc;
        }
    }
    free(seen_gpc);
    free(seen_used);
    if (valid && stage != NULL) *stage = 2;
    for (uint64_t i = 0; valid && i < layout->owners; i++) {
        const uint8_t *record = layout->owner_records + i * X64_PC_OWNER_SIZE;
        uint32_t start = x64_pc_get32(record), end = x64_pc_get32(record + 4);
        uint32_t preserve = x64_pc_get32(record + 8), reserved = x64_pc_get32(record + 12);
        uint32_t map_ordinal = x64_pc_get32(record + 24);
        valid = start < end && end <= layout->arena && reserved == 0 &&
                (preserve & ~policy->owner_preserve_mask) == 0 &&
                (map_ordinal == UINT32_MAX || map_ordinal < layout->maps);
        if (valid && i != 0)
            valid = x64_pc_get32(record - X64_PC_OWNER_SIZE + 4) <= start;
        if (valid && map_ordinal != UINT32_MAX) {
            const uint8_t *map = layout->map_records + (uint64_t)map_ordinal * X64_PC_MAP_SIZE;
            uint64_t host = x64_pc_get64(map + 24);
            uint64_t slice_end = map_ordinal + 1 < layout->maps
                                     ? x64_pc_get64(map + X64_PC_MAP_SIZE + 24)
                                     : layout->arena;
            valid = start >= host && end <= slice_end && i >= x64_pc_get32(map + 84) &&
                    i < (uint64_t)x64_pc_get32(map + 84) + x64_pc_get32(map + 88);
        }
    }
    return valid;
}

static int x64_pc_nonempty(const uint8_t *bytes, size_t size) {
    uint8_t any = 0;
    for (size_t i = 0; i < size; i++) any |= bytes[i];
    return any != 0;
}

static int x64_pc_span_valid(uint64_t base, uint64_t length, uint64_t *end) {
    if (length == 0 || length > UINT64_MAX - base) return 0;
    *end = base + length;
    return 1;
}

static int x64_pc_inside_span(uint64_t lo, uint64_t hi, uint64_t base, uint64_t length) {
    uint64_t end;
    return lo < hi && x64_pc_span_valid(base, length, &end) && lo >= base && hi <= end;
}

int x64_pc_validate_relocations_authority(const x64_pc_format_layout *layout,
                                          x64_pc_external_authority external, void *context,
                                          unsigned *stage) {
    x64_pc_gpc_index_entry *gpc_index = layout->maps == 0 ? NULL
        : malloc((size_t)layout->maps * sizeof *gpc_index);
    int valid = layout->maps == 0 || gpc_index != NULL;
    if (valid) x64_pc_gpc_index_build(layout->map_records, layout->maps, gpc_index);
    uint32_t prior = 0;
    if (stage != NULL) *stage = 3;
    for (uint64_t i = 0; valid && i < layout->relocations; i++) {
        const uint8_t *record = layout->relocation_records + i * X64_PC_RELOC_SIZE;
        uint32_t offset = x64_pc_get32(record), kind = x64_pc_get32(record + 4);
        valid = offset <= layout->arena && layout->arena - offset >= 8 && external(context, kind) &&
                (i == 0 || offset > prior) && offset >= 2 && layout->arena_bytes[offset - 2] == 0x48 &&
                layout->arena_bytes[offset - 1] == 0xb8;
        prior = offset;
    }
    prior = 0;
    for (uint64_t i = 0; valid && i < layout->helper_relocations; i++) {
        const uint8_t *record = layout->helper_relocation_records + i * X64_PC_HELPER_RELOC_SIZE;
        uint32_t offset = x64_pc_get32(record), encoded = x64_pc_get32(record + 4);
        uint32_t form = encoded & ~UINT32_C(1);
        uint32_t length = form == 0 ? 5 : form == X64_PC_HELPER_RELOC_LEA ? 7 : 0;
        valid = length != 0 && offset <= layout->arena && layout->arena - offset >= length &&
                (i == 0 || offset > prior);
        if (valid)
            valid = length == 5
                        ? layout->arena_bytes[offset] == 0xe9
                        : layout->arena_bytes[offset] == 0x48 && layout->arena_bytes[offset + 1] == 0x8d &&
                              layout->arena_bytes[offset + 2] == 0x05;
        prior = offset;
    }
    if (valid && stage != NULL) *stage = 4;
    for (uint64_t i = 0; valid && i < layout->libraries; i++) {
        const uint8_t *record = layout->library_records + i * X64_PC_LIB_SIZE;
        uint64_t base = x64_pc_get64(record), length = x64_pc_get64(record + 8), end;
        valid = x64_pc_span_valid(base, length, &end) && base >= X64_PC_LIB_BASE &&
                end <= X64_PC_LIB_BASE + X64_PC_LIB_SPAN && x64_pc_get64(record + 16) != 0 &&
                x64_pc_nonempty(record + 24, 32) &&
                (i == 0 || x64_pc_get64(record - X64_PC_LIB_SIZE) +
                               x64_pc_get64(record - X64_PC_LIB_SIZE + 8) <= base);
    }
    if (valid && stage != NULL) *stage = 5;
    uint32_t prior_chain = 0;
    for (uint64_t i = 0; valid && i < layout->chains; i++) {
        const uint8_t *record = layout->chain_records + i * X64_PC_CHAIN_SIZE;
        uint32_t site = x64_pc_get32(record), fallback = x64_pc_get32(record + 4);
        uint64_t target = x64_pc_get64(record + 16);
        const uint8_t *candidate = x64_pc_gpc_index_find(target, layout->map_records, gpc_index, layout->maps);
        uint64_t target_entry = candidate == NULL ? UINT64_MAX
            : x64_pc_get64(candidate + 24) + x64_pc_get32(candidate + 72);
        int32_t displacement = 0;
        if (site <= layout->arena && layout->arena - site >= 5)
            memcpy(&displacement, layout->arena_bytes + site + 1, sizeof displacement);
        int64_t destination = (int64_t)site + 5 + displacement;
        valid = site <= layout->arena && layout->arena - site >= 5 && fallback < layout->arena &&
                fallback == site + 5 && layout->arena_bytes[site] == 0xe9 && target_entry < layout->arena &&
                destination >= 0 && ((uint64_t)destination == fallback || (uint64_t)destination == target_entry) &&
                (i == 0 || site > prior_chain);
        prior_chain = site;
    }
    if (valid && stage != NULL) *stage = 6;
    uint64_t chain_cursor = 0;
    for (uint64_t i = 0; valid && i < layout->maps; i++) {
        const uint8_t *map = layout->map_records + i * X64_PC_MAP_SIZE;
        uint32_t owner_start = x64_pc_get32(map + 84), owner_count = x64_pc_get32(map + 88);
        uint32_t chain_start = x64_pc_get32(map + 92), chain_count = x64_pc_get32(map + 96);
        valid = chain_start == chain_cursor && chain_count <= layout->chains - chain_cursor;
        chain_cursor += chain_count;
        if (i != 0) {
            const uint8_t *previous = map - X64_PC_MAP_SIZE;
            valid = valid &&
                    x64_pc_get32(previous + 84) + x64_pc_get32(previous + 88) <= owner_start &&
                    x64_pc_get32(previous + 92) + x64_pc_get32(previous + 96) <= chain_start;
        }
        for (uint32_t j = 0; valid && j < owner_count; j++)
            valid = x64_pc_get32(layout->owner_records +
                                 (uint64_t)(owner_start + j) * X64_PC_OWNER_SIZE + 24) == i;
        for (uint32_t j = 0; valid && j < chain_count; j++) {
            const uint8_t *chain = layout->chain_records +
                                   (uint64_t)(chain_start + j) * X64_PC_CHAIN_SIZE;
            uint32_t site = x64_pc_get32(chain), fallback = x64_pc_get32(chain + 4);
            uint64_t slice_start = x64_pc_get64(map + 24);
            uint64_t slice_end = i + 1 < layout->maps
                                     ? x64_pc_get64(map + X64_PC_MAP_SIZE + 24)
                                     : layout->arena;
            int32_t displacement;
            memcpy(&displacement, layout->arena_bytes + site + 1, sizeof displacement);
            int64_t destination = (int64_t)site + 5 + displacement;
            valid = site >= slice_start && site <= slice_end && slice_end - site >= 5 &&
                    fallback == site + 5 && fallback < slice_end && destination >= 0 &&
                    (uint64_t)destination < layout->arena &&
                    x64_pc_get64(chain + 8) >= x64_pc_get64(map + 8) &&
                    x64_pc_get64(chain + 8) < x64_pc_get64(map + 16);
        }
    }
    valid = valid && chain_cursor == layout->chains;
    if (valid && stage != NULL) *stage = 7;
    for (uint64_t i = 0; valid && i < layout->maps; i++) {
        const uint8_t *map = layout->map_records + i * X64_PC_MAP_SIZE;
        uint64_t lo = x64_pc_get64(map + 8), hi = x64_pc_get64(map + 16);
        int authority = (lo >= layout->image_lo && hi <= layout->image_hi) ||
                        (lo >= layout->interpreter_lo && hi <= layout->interpreter_hi);
        for (uint64_t j = 0; !authority && j < layout->libraries; j++) {
            const uint8_t *library = layout->library_records + j * X64_PC_LIB_SIZE;
            authority = x64_pc_inside_span(lo, hi, x64_pc_get64(library), x64_pc_get64(library + 8));
        }
        valid = authority;
    }
    free(gpc_index);
    return valid;
}

int x64_pc_artifact_name(const hl_host_services *services, const char *directory,
                         const uint8_t identity[32], char *name, size_t size) {
    if (directory == NULL || directory[0] == 0 || name == NULL || size < 76) return 0;
    if (x64_pc_directory.handle != HL_HOST_HANDLE_INVALID &&
        strcmp(x64_pc_directory_path, directory) != 0) {
        (void)hl_persist_directory_close(&x64_pc_directory);
        x64_pc_directory_path[0] = 0;
    }
    if (x64_pc_directory.handle == HL_HOST_HANDLE_INVALID &&
        !hl_persist_directory_open(&x64_pc_directory, services, directory, 1))
        return 0;
    if (!x64_pc_directory_path[0]) {
        int copied = snprintf(x64_pc_directory_path, sizeof x64_pc_directory_path, "%s", directory);
        if (copied <= 0 || (size_t)copied >= sizeof x64_pc_directory_path) return 0;
    }
    static const char hex[] = "0123456789abcdef";
    for (size_t i = 0; i < 32; i++) {
        name[i * 2] = hex[identity[i] >> 4];
        name[i * 2 + 1] = hex[identity[i] & 15];
    }
    memcpy(name + 64, ".x64pcache", 11);
    return 1;
}

int x64_pc_artifact_load(const char *name, uint64_t limit, void **data, size_t *size) {
    return hl_persist_load_at(&x64_pc_directory, name, limit, data, size);
}

int x64_pc_artifact_store(const char *name, const void *data, size_t size) {
    return hl_persist_store_at(&x64_pc_directory, name, data, size);
}

void x64_pc_artifact_close(void) {
    if (x64_pc_directory.handle != HL_HOST_HANDLE_INVALID)
        (void)hl_persist_directory_close(&x64_pc_directory);
    x64_pc_directory_path[0] = 0;
}
