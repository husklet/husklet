#include "image.h"

#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

static uint16_t image_u16(const uint8_t *bytes) {
    uint16_t value;
    memcpy(&value, bytes, sizeof value);
    return value;
}

static uint32_t image_u32(const uint8_t *bytes) {
    uint32_t value;
    memcpy(&value, bytes, sizeof value);
    return value;
}

static uint64_t image_u64(const uint8_t *bytes) {
    uint64_t value;
    memcpy(&value, bytes, sizeof value);
    return value;
}

static int image_range(uint64_t offset, uint64_t length, size_t size) {
    return offset <= size && length <= (uint64_t)size - offset;
}

int hl_linux_elf64_validate(const hl_linux_image *image, uint16_t expected_machine, hl_linux_elf64_layout *layout) {
    enum { ELF_HEADER_SIZE = 64, PROGRAM_HEADER_SIZE = 56, MAX_PROGRAM_HEADERS = 1024, MAX_LOAD_SEGMENTS = 128 };
    uint64_t program_offset, table_size, load_start = UINT64_MAX, load_end = 0;
    uint16_t program_count, program_size, type, entry_is_executable = 0;
    uint64_t entry;
    unsigned loads = 0, nonempty_loads = 0, interpreters = 0;
    if (layout != NULL) memset(layout, 0, sizeof *layout);
    if (image == NULL || image->bytes == NULL || layout == NULL || image->size < ELF_HEADER_SIZE) return -1;
    const uint8_t *bytes = image->bytes;
    if (memcmp(bytes, "\177ELF\2\1\1", 7) != 0 || image_u32(bytes + 20) != 1 || image_u16(bytes + 52) != ELF_HEADER_SIZE)
        return -1;
    type = image_u16(bytes + 16);
    entry = image_u64(bytes + 24);
    if ((type != 2 && type != 3) || image_u16(bytes + 18) != expected_machine) return -1;
    program_offset = image_u64(bytes + 32);
    program_size = image_u16(bytes + 54);
    program_count = image_u16(bytes + 56);
    if (program_size != PROGRAM_HEADER_SIZE || program_count == 0 || program_count > MAX_PROGRAM_HEADERS) return -1;
    table_size = (uint64_t)program_size * program_count;
    if (!image_range(program_offset, table_size, image->size)) return -1;
    for (uint16_t index = 0; index < program_count; ++index) {
        const uint8_t *program = bytes + program_offset + (uint64_t)index * program_size;
        uint32_t kind = image_u32(program);
        uint64_t offset = image_u64(program + 8), address = image_u64(program + 16);
        uint64_t file_size = image_u64(program + 32), memory_size = image_u64(program + 40);
        uint64_t alignment = image_u64(program + 48);
        if (kind == 1) {
            if (++loads > MAX_LOAD_SEGMENTS || file_size > memory_size || !image_range(offset, file_size, image->size) ||
                memory_size > UINT64_MAX - address ||
                (alignment > 1 && ((alignment & (alignment - 1)) != 0 || address % alignment != offset % alignment)))
                return -1;
            if (memory_size == 0) continue;
            ++nonempty_loads;
            if (address < load_start) load_start = address;
            if (address + memory_size > load_end) load_end = address + memory_size;
            if ((image_u32(program + 4) & 1) != 0 && entry >= address && entry < address + memory_size)
                entry_is_executable = 1;
        } else if (kind == 3) {
            if (++interpreters > 1 || file_size == 0 || file_size > 4096 ||
                !image_range(offset, file_size, image->size) ||
                bytes[offset + file_size - 1] != 0 || memchr(bytes + offset, 0, (size_t)file_size - 1) != NULL)
                return -1;
        }
    }
    if (nonempty_loads == 0 || load_end <= load_start || !entry_is_executable || load_end > UINT64_MAX - 0xffff)
        return -1;
    *layout =
        (hl_linux_elf64_layout){program_offset, load_start, load_end, program_count, program_size, type, entry_is_executable};
    return 0;
}

void hl_linux_image_release(hl_linux_image *image) {
    if (image == NULL) return;
    free(image->bytes);
    image->bytes = NULL;
    image->size = 0;
}

int hl_linux_image_read_bytes(const void *source, size_t size, hl_linux_image *image) {
    if (image == NULL) return -1;
    *image = (hl_linux_image){0};
    if (source == NULL || size == 0) return -1;
    image->bytes = malloc(size);
    if (image->bytes == NULL) return -1;
    memcpy(image->bytes, source, size);
    image->size = size;
    return 0;
}

int hl_linux_image_read(const hl_host_services *host, const char *path, hl_linux_image *image) {
    const hl_host_file_services *file;
    hl_host_result opened;
    hl_host_file_metadata metadata;
    uint8_t *bytes = NULL;
    uint64_t offset = 0;
    int result = -1;

    if (image == NULL) return -1;
    *image = (hl_linux_image){0};
    if (host == NULL || path == NULL || host->file == NULL) return -1;
    file = host->file;
    if (file->open_relative == NULL || file->metadata == NULL || file->read_at == NULL || file->close == NULL)
        return -1;

    opened = file->open_relative(host->context, HL_HOST_HANDLE_CWD, path, strlen(path), HL_HOST_FILE_READ, 0, 0);
    if (opened.status != HL_STATUS_OK) return -1;
    memset(&metadata, 0, sizeof(metadata));
    if (file->metadata(host->context, opened.value, &metadata).status != HL_STATUS_OK ||
        metadata.type != HL_HOST_FILE_TYPE_REGULAR || metadata.size == 0 || metadata.size > SIZE_MAX)
        goto done;
    bytes = malloc((size_t)metadata.size);
    if (bytes == NULL) goto done;
    while (offset < metadata.size) {
        hl_host_result read =
            file->read_at(host->context, opened.value, offset, (hl_host_bytes){bytes + offset, metadata.size - offset});
        if (read.status != HL_STATUS_OK || read.value == 0 || read.value > metadata.size - offset) goto done;
        offset += read.value;
    }
    image->bytes = bytes;
    image->size = (size_t)metadata.size;
    bytes = NULL;
    result = 0;

done:
    free(bytes);
    if (file->close(host->context, opened.value).status != HL_STATUS_OK) {
        hl_linux_image_release(image);
        result = -1;
    }
    return result;
}

int hl_linux_image_read_handle(const hl_host_services *host, hl_host_handle handle, hl_linux_image *image) {
    const hl_host_file_services *file;
    hl_host_file_metadata metadata;
    uint8_t *bytes = NULL;
    uint64_t offset = 0;
    if (image == NULL) return -1;
    *image = (hl_linux_image){0};
    if (host == NULL || host->file == NULL || handle == HL_HOST_HANDLE_INVALID) return -1;
    file = host->file;
    if (file->metadata == NULL || file->read_at == NULL) return -1;
    memset(&metadata, 0, sizeof(metadata));
    if (file->metadata(host->context, handle, &metadata).status != HL_STATUS_OK ||
        metadata.type != HL_HOST_FILE_TYPE_REGULAR || metadata.size == 0 || metadata.size > SIZE_MAX)
        return -1;
    bytes = malloc((size_t)metadata.size);
    if (bytes == NULL) return -1;
    while (offset < metadata.size) {
        hl_host_result read =
            file->read_at(host->context, handle, offset, (hl_host_bytes){bytes + offset, metadata.size - offset});
        if (read.status != HL_STATUS_OK || read.value == 0 || read.value > metadata.size - offset) {
            free(bytes);
            return -1;
        }
        offset += read.value;
    }
    image->bytes = bytes;
    image->size = (size_t)metadata.size;
    return 0;
}

int hl_linux_image_read_fd(int descriptor, hl_linux_image *image) {
    struct stat metadata;
    uint8_t *bytes;
    size_t offset = 0;
    if (image == NULL) return -1;
    *image = (hl_linux_image){0};
    if (descriptor < 0 || fstat(descriptor, &metadata) != 0 || !S_ISREG(metadata.st_mode) || metadata.st_size <= 0 ||
        (uintmax_t)metadata.st_size > SIZE_MAX)
        return -1;
    bytes = malloc((size_t)metadata.st_size);
    if (bytes == NULL) return -1;
    if (lseek(descriptor, 0, SEEK_SET) < 0) {
        free(bytes);
        return -1;
    }
    while (offset < (size_t)metadata.st_size) {
        ssize_t count = read(descriptor, bytes + offset, (size_t)metadata.st_size - offset);
        if (count < 0 && errno == EINTR) continue;
        if (count <= 0) {
            free(bytes);
            return -1;
        }
        offset += (size_t)count;
    }
    image->bytes = bytes;
    image->size = (size_t)metadata.st_size;
    return 0;
}
