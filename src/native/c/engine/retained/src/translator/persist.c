#include "persist.h"

#include <stdlib.h>
#include <string.h>

static int hl_persist_file_abi(const hl_host_services *services) {
    return services != NULL && services->file != NULL && services->file->abi == HL_HOST_FILE_ABI &&
           services->file->size >= sizeof(*services->file);
}

static int hl_persist_leaf(const char *name, size_t *size) {
    const char *cursor;
    if (name == NULL || name[0] == 0 || size == NULL) return 0;
    for (cursor = name; *cursor != 0; cursor++)
        if (*cursor == '/') return 0;
    if ((cursor - name == 1 && name[0] == '.') || (cursor - name == 2 && name[0] == '.' && name[1] == '.')) return 0;
    *size = (size_t)(cursor - name);
    return 1;
}

int hl_persist_directory_open(hl_persist_directory *directory, const hl_host_services *services, const char *path,
                              int create) {
    hl_host_result result;
    size_t path_size;
    if (directory != NULL) *directory = (hl_persist_directory){0};
    if (directory == NULL || !hl_persist_file_abi(services) || path == NULL || path[0] == 0 ||
        services->file->open_relative == NULL || services->file->close == NULL ||
        services->file->validate_private_directory == NULL || (create && services->file->make_directory == NULL))
        return 0;
    path_size = strlen(path);
    if (create) (void)services->file->make_directory(services->context, HL_HOST_HANDLE_CWD, path, path_size, 0700);
    result = services->file->open_relative(services->context, HL_HOST_HANDLE_CWD, path, path_size,
                                           HL_HOST_FILE_READ | HL_HOST_FILE_DIRECTORY | HL_HOST_FILE_NOFOLLOW, 0, 0);
    if (result.status != HL_STATUS_OK) return 0;
    if (services->file->validate_private_directory(services->context, result.value).status != HL_STATUS_OK) {
        (void)services->file->close(services->context, result.value);
        return 0;
    }
    directory->services = services;
    directory->handle = result.value;
    return 1;
}

int hl_persist_directory_close(hl_persist_directory *directory) {
    hl_host_result result;
    if (directory == NULL || directory->services == NULL || directory->handle == HL_HOST_HANDLE_INVALID) return 0;
    result = directory->services->file->close(directory->services->context, directory->handle);
    *directory = (hl_persist_directory){0};
    return result.status == HL_STATUS_OK;
}

int hl_persist_load_at(const hl_persist_directory *directory, const char *name, uint64_t limit, void **data,
                       size_t *size) {
    hl_host_result opened, result;
    size_t name_size;
    hl_host_file_metadata metadata;
    unsigned char *bytes = NULL;
    uint64_t offset = 0;
    int ok = 0;
    if (data != NULL) *data = NULL;
    if (size != NULL) *size = 0;
    if (directory == NULL || directory->services == NULL || directory->handle == HL_HOST_HANDLE_INVALID ||
        data == NULL || size == NULL || !hl_persist_leaf(name, &name_size))
        return 0;
    const hl_host_services *services = directory->services;
    if (services->file->open_relative == NULL || services->file->read_at == NULL || services->file->metadata == NULL ||
        services->file->close == NULL || services->file->validate_private_regular == NULL)
        return 0;
    opened = services->file->open_relative(services->context, directory->handle, name, name_size,
                                           HL_HOST_FILE_READ | HL_HOST_FILE_NOFOLLOW, 0, 0);
    if (opened.status != HL_STATUS_OK) return 0;
    result = services->file->validate_private_regular(services->context, opened.value);
    if (result.status != HL_STATUS_OK) goto done;
    result = services->file->metadata(services->context, opened.value, &metadata);
    if (result.status != HL_STATUS_OK || metadata.type != HL_HOST_FILE_TYPE_REGULAR || metadata.size > limit ||
        metadata.size > SIZE_MAX)
        goto done;
    bytes = metadata.size != 0 ? malloc((size_t)metadata.size) : malloc(1);
    if (bytes == NULL) goto done;
    while (offset < metadata.size) {
        result = services->file->read_at(services->context, opened.value, offset,
                                         (hl_host_bytes){bytes + offset, (size_t)(metadata.size - offset)});
        if (result.status != HL_STATUS_OK || result.value == 0 || result.value > metadata.size - offset) goto done;
        offset += result.value;
    }
    ok = 1;
done:
    if (services->file->close(services->context, opened.value).status != HL_STATUS_OK) ok = 0;
    if (!ok) {
        free(bytes);
        return 0;
    }
    *data = bytes;
    *size = (size_t)metadata.size;
    return 1;
}

int hl_persist_store_at(const hl_persist_directory *directory, const char *name, const void *data, size_t size) {
    size_t name_size;
    if (directory == NULL || directory->services == NULL || directory->handle == HL_HOST_HANDLE_INVALID ||
        (size != 0 && data == NULL) || !hl_persist_leaf(name, &name_size) ||
        directory->services->file->store_private_atomic == NULL)
        return 0;
    return directory->services->file
               ->store_private_atomic(directory->services->context, directory->handle, name, name_size,
                                      (hl_host_const_bytes){data, size}, 0600)
               .status == HL_STATUS_OK;
}

int hl_persist_remove_at(const hl_persist_directory *directory, const char *name) {
    size_t name_size;
    if (directory == NULL || directory->services == NULL || directory->handle == HL_HOST_HANDLE_INVALID ||
        !hl_persist_leaf(name, &name_size) || directory->services->file->unlink_relative == NULL)
        return 0;
    return directory->services->file->unlink_relative(directory->services->context, directory->handle, name, name_size)
               .status == HL_STATUS_OK;
}

int hl_persist_take(hl_persist_cursor *cursor, void *output, size_t size) {
    if (cursor == NULL || (size != 0 && output == NULL) || cursor->offset > cursor->size ||
        size > cursor->size - cursor->offset)
        return 0;
    if (size != 0) memcpy(output, cursor->data + cursor->offset, size);
    cursor->offset += size;
    return 1;
}
