#include "arena_sidecar.h"

#include <errno.h>
#include <string.h>

enum {
    HEADER_CHECKSUM = 24,
    HEADER_ISA = 32,
    HEADER_RECORD_SIZE = 36,
    HEADER_RECORD_COUNT = 40,
    HEADER_RESERVED = 44,
    HEADER_GRANULE = 48,
    HEADER_NONCE = 56,
    HEADER_AUTHORITY = 64,
    HEADER_GENERATION = 72,
};

static void put_u32(unsigned char *output, uint32_t value) {
    for (unsigned index = 0; index < 4; ++index) output[index] = (unsigned char)(value >> (8 * index));
}

static void put_u64(unsigned char *output, uint64_t value) {
    for (unsigned index = 0; index < 8; ++index) output[index] = (unsigned char)(value >> (8 * index));
}

static uint32_t get_u32(const unsigned char *input) {
    uint32_t value = 0;
    for (unsigned index = 0; index < 4; ++index) value |= (uint32_t)input[index] << (8 * index);
    return value;
}

static uint64_t get_u64(const unsigned char *input) {
    uint64_t value = 0;
    for (unsigned index = 0; index < 8; ++index) value |= (uint64_t)input[index] << (8 * index);
    return value;
}

static uint64_t checksum(const unsigned char *bytes, size_t size) {
    uint64_t hash = UINT64_C(14695981039346656037);
    for (size_t index = 0; index < size; ++index) {
        const unsigned char byte = index >= HEADER_CHECKSUM && index < HEADER_CHECKSUM + 8 ? 0 : bytes[index];
        hash = (hash ^ byte) * UINT64_C(1099511628211);
    }
    return hash;
}

int hl_arena_mapping_sidecar_size(uint32_t record_count, size_t *size) {
    if (size == NULL || record_count > HL_ARENA_SIDECAR_MAX_RECORDS) return (errno = EINVAL, -1);
    *size = (size_t)HL_ARENA_SIDECAR_HEADER_SIZE + (size_t)record_count * HL_ARENA_SIDECAR_RECORD_SIZE;
    return 0;
}

static int protection_valid(uint32_t protection) {
    const uint32_t allowed = HL_ARENA_PROTECTION_READ | HL_ARENA_PROTECTION_WRITE | HL_ARENA_PROTECTION_EXECUTE;
    return (protection & HL_ARENA_PROTECTION_READ) != 0 && (protection & ~allowed) == 0;
}

static int sidecar_valid(const hl_arena_mapping_sidecar *sidecar) {
    if (sidecar == NULL || (sidecar->guest_isa != 1 && sidecar->guest_isa != 2) ||
        sidecar->record_count > HL_ARENA_SIDECAR_MAX_RECORDS || sidecar->granule == 0 ||
        (sidecar->granule & (sidecar->granule - 1)) != 0 || sidecar->authority_nonce == 0 ||
        sidecar->authority_identity == 0 || sidecar->generation == 0)
        return 0;
    for (uint32_t index = 0; index < sidecar->record_count; ++index) {
        const hl_arena_mapping_source *record = &sidecar->records[index];
        if (record->reservation_identity == 0 || record->address == 0 || record->length == 0 ||
            record->address % sidecar->granule != 0 || record->length % sidecar->granule != 0 ||
            record->address > UINT64_MAX - record->length || !protection_valid(record->protection) ||
            record->flags != 0 || record->reserved != 0 ||
            (record->source_kind != HL_ARENA_MAPPING_ANONYMOUS && record->source_kind != HL_ARENA_MAPPING_FILE))
            return 0;
        if (record->source_kind == HL_ARENA_MAPPING_ANONYMOUS) {
            if (record->source_identity != 0 || record->source_offset != 0) return 0;
        } else if (record->source_identity == 0 || record->source_offset % sidecar->granule != 0 ||
                   record->source_offset > UINT64_MAX - record->length) {
            return 0;
        }
        for (uint32_t previous = 0; previous < index; ++previous) {
            const hl_arena_mapping_source *other = &sidecar->records[previous];
            if (other->reservation_identity == record->reservation_identity ||
                (record->address < other->address + other->length && other->address < record->address + record->length))
                return 0;
        }
    }
    return 1;
}

int hl_arena_mapping_sidecar_encode(const hl_arena_mapping_sidecar *sidecar, void *output, size_t capacity,
                                    size_t *written) {
    size_t size;
    if (written != NULL) *written = 0;
    if (output == NULL || written == NULL || !sidecar_valid(sidecar) ||
        hl_arena_mapping_sidecar_size(sidecar != NULL ? sidecar->record_count : 0, &size) != 0)
        return (errno = EINVAL, -1);
    if (capacity < size) return (errno = ENOSPC, -1);
    unsigned char *bytes = output;
    memset(bytes, 0, size);
    put_u64(bytes, HL_ARENA_SIDECAR_MAGIC);
    put_u32(bytes + 8, HL_ARENA_SIDECAR_VERSION);
    put_u32(bytes + 12, HL_ARENA_SIDECAR_HEADER_SIZE);
    put_u64(bytes + 16, size);
    put_u32(bytes + HEADER_ISA, sidecar->guest_isa);
    put_u32(bytes + HEADER_RECORD_SIZE, HL_ARENA_SIDECAR_RECORD_SIZE);
    put_u32(bytes + HEADER_RECORD_COUNT, sidecar->record_count);
    put_u64(bytes + HEADER_GRANULE, sidecar->granule);
    put_u64(bytes + HEADER_NONCE, sidecar->authority_nonce);
    put_u64(bytes + HEADER_AUTHORITY, sidecar->authority_identity);
    put_u64(bytes + HEADER_GENERATION, sidecar->generation);
    for (uint32_t index = 0; index < sidecar->record_count; ++index) {
        unsigned char *record = bytes + HL_ARENA_SIDECAR_HEADER_SIZE + (size_t)index * HL_ARENA_SIDECAR_RECORD_SIZE;
        const hl_arena_mapping_source *source = &sidecar->records[index];
        put_u64(record, source->reservation_identity);
        put_u64(record + 8, source->address);
        put_u64(record + 16, source->length);
        put_u64(record + 24, source->source_identity);
        put_u64(record + 32, source->source_offset);
        put_u32(record + 40, source->source_kind);
        put_u32(record + 44, source->protection);
        put_u32(record + 48, source->flags);
        put_u32(record + 52, source->reserved);
    }
    put_u64(bytes + HEADER_CHECKSUM, checksum(bytes, size));
    *written = size;
    return 0;
}

int hl_arena_mapping_sidecar_parse(const void *input, size_t size, const hl_arena_sidecar_authority *expected,
                                   hl_arena_mapping_sidecar *sidecar) {
    const unsigned char *bytes = input;
    size_t expected_size;
    if (sidecar != NULL) memset(sidecar, 0, sizeof(*sidecar));
    if (input == NULL || sidecar == NULL || expected == NULL || expected->reserved != 0 ||
        (expected->guest_isa != 1 && expected->guest_isa != 2) || expected->granule == 0 ||
        expected->authority_nonce == 0 || expected->authority_identity == 0 || expected->generation == 0 ||
        expected->mapping_count > HL_ARENA_SIDECAR_MAX_RECORDS || expected->mapping_reserved != 0 ||
        (expected->mapping_count != 0 && expected->mappings == NULL) ||
        size < HL_ARENA_SIDECAR_HEADER_SIZE)
        return (errno = EINVAL, -1);
    const uint32_t count = get_u32(bytes + HEADER_RECORD_COUNT);
    if (get_u64(bytes) != HL_ARENA_SIDECAR_MAGIC || get_u32(bytes + 8) != HL_ARENA_SIDECAR_VERSION ||
        get_u32(bytes + 12) != HL_ARENA_SIDECAR_HEADER_SIZE ||
        get_u32(bytes + HEADER_RECORD_SIZE) != HL_ARENA_SIDECAR_RECORD_SIZE ||
        get_u32(bytes + HEADER_RESERVED) != 0 || count > HL_ARENA_SIDECAR_MAX_RECORDS ||
        count != expected->mapping_count || hl_arena_mapping_sidecar_size(count, &expected_size) != 0 ||
        size != expected_size || get_u64(bytes + 16) != size ||
        get_u64(bytes + HEADER_CHECKSUM) != checksum(bytes, size))
        return (errno = EINVAL, -1);
    sidecar->guest_isa = get_u32(bytes + HEADER_ISA);
    sidecar->record_count = count;
    sidecar->granule = get_u64(bytes + HEADER_GRANULE);
    sidecar->authority_nonce = get_u64(bytes + HEADER_NONCE);
    sidecar->authority_identity = get_u64(bytes + HEADER_AUTHORITY);
    sidecar->generation = get_u64(bytes + HEADER_GENERATION);
    if (sidecar->guest_isa != expected->guest_isa || sidecar->granule != expected->granule ||
        sidecar->authority_nonce != expected->authority_nonce ||
        sidecar->authority_identity != expected->authority_identity || sidecar->generation != expected->generation) {
        memset(sidecar, 0, sizeof(*sidecar));
        return (errno = EACCES, -1);
    }
    for (uint32_t index = 0; index < count; ++index) {
        const unsigned char *record =
            bytes + HL_ARENA_SIDECAR_HEADER_SIZE + (size_t)index * HL_ARENA_SIDECAR_RECORD_SIZE;
        hl_arena_mapping_source *target = &sidecar->records[index];
        target->reservation_identity = get_u64(record);
        target->address = get_u64(record + 8);
        target->length = get_u64(record + 16);
        target->source_identity = get_u64(record + 24);
        target->source_offset = get_u64(record + 32);
        target->source_kind = get_u32(record + 40);
        target->protection = get_u32(record + 44);
        target->flags = get_u32(record + 48);
        target->reserved = get_u32(record + 52);
        if (target->reservation_identity != expected->mappings[index].reservation_identity ||
            target->address != expected->mappings[index].address || target->length != expected->mappings[index].length) {
            memset(sidecar, 0, sizeof(*sidecar));
            return (errno = EACCES, -1);
        }
    }
    if (!sidecar_valid(sidecar)) {
        memset(sidecar, 0, sizeof(*sidecar));
        return (errno = EINVAL, -1);
    }
    return 0;
}

int hl_arena_mapping_sidecar_publish(const hl_arena_mapping_sidecar *sidecar, void *scratch, size_t capacity,
                                     const hl_arena_sidecar_publication *publication) {
    size_t size = 0;
    if (publication == NULL || publication->begin == NULL || publication->write == NULL ||
        publication->commit == NULL || publication->abort == NULL)
        return (errno = EINVAL, -1);
    if (hl_arena_mapping_sidecar_encode(sidecar, scratch, capacity, &size) != 0) return -1;
    errno = 0;
    if (publication->begin(publication->context, size) != 0) return (errno = errno != 0 ? errno : EIO, -1);
    errno = 0;
    if (publication->write(publication->context, scratch, size) != 0) {
        int publication_error = errno != 0 ? errno : EIO;
        publication->abort(publication->context);
        errno = publication_error;
        return -1;
    }
    errno = 0;
    if (publication->commit(publication->context) != 0) {
        int publication_error = errno != 0 ? errno : EIO;
        publication->abort(publication->context);
        errno = publication_error;
        return -1;
    }
    return 0;
}
