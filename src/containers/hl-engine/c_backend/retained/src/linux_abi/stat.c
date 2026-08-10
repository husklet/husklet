#include "hl/linux_abi.h"
#include "encode.h"

#include <limits.h>
#include <string.h>

static void hl_stat_u32(uint8_t *output, size_t offset, uint32_t value) {
    output[offset + 0] = (uint8_t)value;
    output[offset + 1] = (uint8_t)(value >> 8);
    output[offset + 2] = (uint8_t)(value >> 16);
    output[offset + 3] = (uint8_t)(value >> 24);
}

static void hl_stat_u64(uint8_t *output, size_t offset, uint64_t value) {
    size_t byte;
    for (byte = 0; byte < 8; byte++)
        output[offset + byte] = (uint8_t)(value >> (byte * 8));
}

static int hl_stat_validate(const hl_linux_stat_record *record, void *output, size_t output_size,
                            size_t required_size) {
    if (record == NULL || output == NULL || output_size < required_size) return -HL_LINUX_EINVAL;
    if (record->size > INT64_MAX || record->blocks_512 > INT64_MAX ||
        record->accessed_nanoseconds >= UINT64_C(1000000000) || record->modified_nanoseconds >= UINT64_C(1000000000) ||
        record->changed_nanoseconds >= UINT64_C(1000000000))
        return -HL_LINUX_EOVERFLOW;
    return 0;
}

int hl_linux_stat_encode_aarch64(const hl_linux_stat_record *record, void *output, size_t output_size) {
    uint8_t *bytes = output;
    int result = hl_stat_validate(record, output, output_size, HL_LINUX_STAT_RECORD_AARCH64_SIZE);
    if (result != 0) return result;
    if (record->links > UINT32_MAX) return -HL_LINUX_EOVERFLOW;
    memset(bytes, 0, HL_LINUX_STAT_RECORD_AARCH64_SIZE);
    hl_stat_u64(bytes, 0, record->device);
    hl_stat_u64(bytes, 8, record->object);
    hl_stat_u32(bytes, 16, record->mode);
    hl_stat_u32(bytes, 20, (uint32_t)record->links);
    hl_stat_u32(bytes, 24, record->user);
    hl_stat_u32(bytes, 28, record->group);
    hl_stat_u64(bytes, 32, record->special_device);
    hl_stat_u64(bytes, 48, record->size);
    hl_stat_u32(bytes, 56, 4096);
    hl_stat_u64(bytes, 64, record->blocks_512);
    hl_stat_u64(bytes, 72, (uint64_t)record->accessed_seconds);
    hl_stat_u64(bytes, 80, record->accessed_nanoseconds);
    hl_stat_u64(bytes, 88, (uint64_t)record->modified_seconds);
    hl_stat_u64(bytes, 96, record->modified_nanoseconds);
    hl_stat_u64(bytes, 104, (uint64_t)record->changed_seconds);
    hl_stat_u64(bytes, 112, record->changed_nanoseconds);
    return 0;
}

int hl_linux_stat_encode_x86_64(const hl_linux_stat_record *record, void *output, size_t output_size) {
    uint8_t *bytes = output;
    int result = hl_stat_validate(record, output, output_size, HL_LINUX_STAT_RECORD_X86_64_SIZE);
    if (result != 0) return result;
    memset(bytes, 0, HL_LINUX_STAT_RECORD_X86_64_SIZE);
    hl_stat_u64(bytes, 0, record->device);
    hl_stat_u64(bytes, 8, record->object);
    hl_stat_u64(bytes, 16, record->links);
    hl_stat_u32(bytes, 24, record->mode);
    hl_stat_u32(bytes, 28, record->user);
    hl_stat_u32(bytes, 32, record->group);
    hl_stat_u64(bytes, 40, record->special_device);
    hl_stat_u64(bytes, 48, record->size);
    hl_stat_u64(bytes, 56, 4096);
    hl_stat_u64(bytes, 64, record->blocks_512);
    hl_stat_u64(bytes, 72, (uint64_t)record->accessed_seconds);
    hl_stat_u64(bytes, 80, record->accessed_nanoseconds);
    hl_stat_u64(bytes, 88, (uint64_t)record->modified_seconds);
    hl_stat_u64(bytes, 96, record->modified_nanoseconds);
    hl_stat_u64(bytes, 104, (uint64_t)record->changed_seconds);
    hl_stat_u64(bytes, 112, record->changed_nanoseconds);
    return 0;
}

static int64_t hl_stat_from_status(const hl_linux_file_status *status, hl_linux_stat_record *record) {
    uint64_t accessed_seconds, modified_seconds, changed_seconds;
    if (status == NULL || record == NULL) return -HL_LINUX_EINVAL;
    accessed_seconds = status->accessed_ns / UINT64_C(1000000000);
    modified_seconds = status->modified_ns / UINT64_C(1000000000);
    changed_seconds = status->changed_ns / UINT64_C(1000000000);
    if (accessed_seconds > INT64_MAX || modified_seconds > INT64_MAX || changed_seconds > INT64_MAX)
        return -HL_LINUX_EOVERFLOW;
    *record = (hl_linux_stat_record){
        .device = status->device,
        .object = status->object,
        .links = status->link_count,
        .special_device = status->special_device,
        .size = status->size,
        .blocks_512 = status->blocks_512,
        .accessed_seconds = (int64_t)accessed_seconds,
        .accessed_nanoseconds = status->accessed_ns % UINT64_C(1000000000),
        .modified_seconds = (int64_t)modified_seconds,
        .modified_nanoseconds = status->modified_ns % UINT64_C(1000000000),
        .changed_seconds = (int64_t)changed_seconds,
        .changed_nanoseconds = status->changed_ns % UINT64_C(1000000000),
        .mode = status->mode,
        .user = status->user,
        .group = status->group,
    };
    return 0;
}

int64_t hl_linux_stat_aarch64(const hl_linux_file_status *status, void *output, size_t output_size) {
    hl_linux_stat_record record;
    int64_t result = hl_stat_from_status(status, &record);
    return result != 0 ? result : hl_linux_stat_encode_aarch64(&record, output, output_size);
}

int64_t hl_linux_stat_x86_64(const hl_linux_file_status *status, void *output, size_t output_size) {
    hl_linux_stat_record record;
    int64_t result = hl_stat_from_status(status, &record);
    return result != 0 ? result : hl_linux_stat_encode_x86_64(&record, output, output_size);
}

int hl_linux_statfs_encode(const hl_linux_statfs_record *record, void *output, size_t output_size) {
    uint8_t *bytes = output;
    if (record == NULL || output == NULL || output_size < HL_LINUX_STATFS_RECORD_SIZE) return -HL_LINUX_EINVAL;
    memset(bytes, 0, HL_LINUX_STATFS_RECORD_SIZE);
    hl_stat_u64(bytes, 0, (uint64_t)record->type);
    hl_stat_u64(bytes, 8, record->block_size);
    hl_stat_u64(bytes, 16, record->blocks);
    hl_stat_u64(bytes, 24, record->blocks_free);
    hl_stat_u64(bytes, 32, record->blocks_available);
    hl_stat_u64(bytes, 40, record->files);
    hl_stat_u64(bytes, 48, record->files_free);
    hl_stat_u32(bytes, 56, record->filesystem_id[0]);
    hl_stat_u32(bytes, 60, record->filesystem_id[1]);
    hl_stat_u64(bytes, 64, record->name_max);
    hl_stat_u64(bytes, 72, record->fragment_size);
    hl_stat_u64(bytes, 80, record->flags);
    return 0;
}
