#include "executable_authority.h"

#include <string.h>

static void hl_c_backend_executable_clear(hl_engine_executable *executable) {
    *executable = (hl_engine_executable){0};
}

HL_API hl_status hl_c_backend_executable_open(const hl_host_services *services, const char *host_path,
                                              hl_engine_executable *output) {
    if (output == NULL) { return HL_STATUS_INVALID_ARGUMENT; }
    hl_c_backend_executable_clear(output);
    if (services == NULL || services->file == NULL || services->file->open_relative == NULL || host_path == NULL ||
        host_path[0] == '\0') {
        return HL_STATUS_INVALID_ARGUMENT;
    }

    hl_host_result opened = services->file->open_relative(services->context, HL_HOST_HANDLE_CWD, host_path,
                                                          strlen(host_path), HL_HOST_FILE_READ, 0, 0);
    if (opened.status != HL_STATUS_OK) { return (hl_status)opened.status; }
    if (opened.value == HL_HOST_HANDLE_INVALID) { return HL_STATUS_CORRUPT; }

    *output = (hl_engine_executable){
        .abi = HL_ENGINE_ABI,
        .size = sizeof(*output),
        .ownership = HL_ENGINE_FD_TRANSFER,
        .reserved = 0,
        .host_handle = opened.value,
        .image = NULL,
        .image_size = 0,
    };
    return HL_STATUS_OK;
}

HL_API void hl_c_backend_executable_discard(const hl_host_services *services, hl_engine_executable *executable) {
    if (executable == NULL) { return; }
    if (executable->host_handle != HL_HOST_HANDLE_INVALID && services != NULL && services->file != NULL &&
        services->file->close != NULL) {
        (void)services->file->close(services->context, executable->host_handle);
    }
    hl_c_backend_executable_clear(executable);
}
