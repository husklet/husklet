#include "image.h"

#include <errno.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

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
