#include "file.h"

#include <errno.h>
#include <string.h>

static int hl_host_file_marker(const hl_host_services *services, const char *path, uint32_t permissions,
                               uint32_t creation) {
    hl_host_result opened;
    if (services == NULL || services->file == NULL || services->file->open_relative == NULL ||
        services->file->close == NULL || path == NULL || path[0] == '\0') {
        errno = EINVAL;
        return -1;
    }
    opened = services->file->open_relative(services->context, HL_HOST_HANDLE_CWD, path, strlen(path),
                                           HL_HOST_FILE_WRITE, creation, permissions);
    if (opened.status != HL_STATUS_OK) {
        errno = EIO;
        return -1;
    }
    (void)services->file->close(services->context, opened.value);
    return 0;
}

int hl_host_file_create(const hl_host_services *services, const char *path, uint32_t permissions) {
    return hl_host_file_marker(services, path, permissions, HL_HOST_FILE_CREATE);
}

int hl_host_file_exclusive(const hl_host_services *services, const char *path, uint32_t permissions) {
    return hl_host_file_marker(services, path, permissions, HL_HOST_FILE_CREATE | HL_HOST_FILE_EXCLUSIVE);
}

int hl_host_file_reset(const hl_host_services *services, const char *path, uint32_t permissions) {
    return hl_host_file_marker(services, path, permissions, HL_HOST_FILE_CREATE | HL_HOST_FILE_TRUNCATE);
}

int hl_host_file_store(const hl_host_services *services, const char *path, uint32_t permissions, const void *data,
                       size_t size) {
    hl_host_result opened;
    size_t offset = 0;
    int failed = 0;
    if (services == NULL || services->file == NULL || services->file->open_relative == NULL ||
        services->file->write == NULL || services->file->close == NULL || path == NULL || path[0] == '\0' ||
        (size != 0 && data == NULL)) {
        errno = EINVAL;
        return -1;
    }
    opened =
        services->file->open_relative(services->context, HL_HOST_HANDLE_CWD, path, strlen(path), HL_HOST_FILE_WRITE,
                                      HL_HOST_FILE_CREATE | HL_HOST_FILE_TRUNCATE, permissions);
    if (opened.status != HL_STATUS_OK) {
        errno = EIO;
        return -1;
    }
    while (offset < size) {
        const unsigned char *bytes = data;
        hl_host_result written =
            services->file->write(services->context, opened.value, bytes + offset, (uint64_t)(size - offset));
        if (written.status != HL_STATUS_OK || written.value == 0 || written.value > size - offset) {
            failed = 1;
            break;
        }
        offset += (size_t)written.value;
    }
    hl_host_result closed = services->file->close(services->context, opened.value);
    if (failed || closed.status != HL_STATUS_OK) {
        errno = EIO;
        return -1;
    }
    return 0;
}

int hl_host_file_load(const hl_host_services *services, const char *path, void *data, size_t size) {
    hl_host_result opened;
    size_t offset = 0;
    int failed = 0;
    if (services == NULL || services->file == NULL || services->file->open_relative == NULL ||
        services->file->read == NULL || services->file->close == NULL || path == NULL || path[0] == '\0' ||
        (size != 0 && data == NULL)) {
        errno = EINVAL;
        return -1;
    }
    opened = services->file->open_relative(services->context, HL_HOST_HANDLE_CWD, path, strlen(path), HL_HOST_FILE_READ,
                                           0, 0);
    if (opened.status != HL_STATUS_OK) {
        errno = EIO;
        return -1;
    }
    while (offset < size) {
        unsigned char *bytes = data;
        hl_host_result read =
            services->file->read(services->context, opened.value, bytes + offset, (uint64_t)(size - offset));
        if (read.status != HL_STATUS_OK || read.value == 0 || read.value > size - offset) {
            failed = 1;
            break;
        }
        offset += (size_t)read.value;
    }
    hl_host_result closed = services->file->close(services->context, opened.value);
    if (failed || closed.status != HL_STATUS_OK) {
        errno = EIO;
        return -1;
    }
    return 0;
}

int hl_host_file_rename(const hl_host_services *services, const char *old_path, const char *new_path) {
    hl_host_result result;
    if (services == NULL || services->file == NULL || services->file->rename_relative == NULL || old_path == NULL ||
        old_path[0] == '\0' || new_path == NULL || new_path[0] == '\0') {
        errno = EINVAL;
        return -1;
    }
    result = services->file->rename_relative(services->context, HL_HOST_HANDLE_CWD, old_path, strlen(old_path),
                                             HL_HOST_HANDLE_CWD, new_path, strlen(new_path));
    if (result.status != HL_STATUS_OK) {
        errno = EIO;
        return -1;
    }
    return 0;
}

int hl_host_file_unlink(const hl_host_services *services, const char *path) {
    hl_host_result result;
    if (services == NULL || services->file == NULL || services->file->unlink_relative == NULL || path == NULL ||
        path[0] == '\0') {
        errno = EINVAL;
        return -1;
    }
    result = services->file->unlink_relative(services->context, HL_HOST_HANDLE_CWD, path, strlen(path));
    if (result.status != HL_STATUS_OK) {
        errno = EIO;
        return -1;
    }
    return 0;
}
