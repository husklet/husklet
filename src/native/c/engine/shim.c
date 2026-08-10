#include "hl/engine.h"
#include "hl/linux.h"
#include "core/engine_backend.h"
#include "core/options.h"
#include "executable_authority.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

/* Explicitly anchors the lifecycle object when the retained backend is linked
 * from static archives. The function is idempotent and replaces reliance on
 * linker-specific constructor extraction. */
extern void hl_target_register_backend(void);

typedef struct hl_c_backend {
    hl_host_linux *host;
    hl_host_services services;
    hl_engine *engine;
    hl_engine_exit result;
} hl_c_backend;

int32_t hl_c_backend_create(uint32_t isa, const char *rootfs, const char *executable_host, uint32_t option_count,
                            const char *const *option_names, const char *const *option_values,
                            const int32_t standard_fds[3],
                            hl_c_backend **output) {
    hl_c_backend *backend;
    hl_engine_config config;
    hl_status status;
    hl_options options;
    uint32_t index;
    hl_engine_fd_binding bindings[3];
    hl_host_result imported[3];
    hl_engine_executable executable;
    if (output == NULL) return HL_STATUS_INVALID_ARGUMENT;
    *output = NULL;
    backend = calloc(1, sizeof(*backend));
    if (backend == NULL) return HL_STATUS_OUT_OF_MEMORY;
    status = hl_host_linux_create(&backend->host, &backend->services);
    if (status != HL_STATUS_OK) {
        free(backend);
        return status;
    }
    memset(&config, 0, sizeof(config));
    memset(&options, 0, sizeof(options));
    hl_target_register_backend();
    config.abi = HL_ENGINE_ABI;
    config.size = sizeof(config);
    config.guest_isa = isa;
    config.rootfs = rootfs;
    memset(bindings, 0, sizeof(bindings));
    memset(imported, 0, sizeof(imported));
    memset(&executable, 0, sizeof(executable));
    if (standard_fds != NULL) {
        for (index = 0; index < 3; ++index) {
            imported[index] = hl_host_linux_import_file(backend->host, standard_fds[index]);
            if (imported[index].status != HL_STATUS_OK) {
                uint32_t close_index;
                for (close_index = 0; close_index < index; ++close_index)
                    (void)backend->services.file->close(backend->services.context, imported[close_index].value);
                hl_host_linux_destroy(backend->host);
                free(backend);
                return imported[index].status;
            }
            bindings[index].abi = HL_ENGINE_ABI;
            bindings[index].size = sizeof(bindings[index]);
            bindings[index].guest_fd = index;
            bindings[index].status_flags = index == 0 ? 0u : 1u;
            bindings[index].ownership = HL_ENGINE_FD_TRANSFER;
            bindings[index].host_handle = imported[index].value;
        }
        config.fd_bindings = bindings;
        config.fd_binding_count = 3;
    }
    if ((option_count != 0 && (option_names == NULL || option_values == NULL)) || hl_options_init(&options) != 0) {
        hl_host_linux_destroy(backend->host);
        free(backend);
        return HL_STATUS_OUT_OF_MEMORY;
    }
    for (index = 0; index < option_count; ++index) {
        if (option_names[index] == NULL || option_values[index] == NULL ||
            hl_options_set(&options, option_names[index], option_values[index], 1) != 0) {
            hl_options_destroy(&options);
            hl_host_linux_destroy(backend->host);
            free(backend);
            return HL_STATUS_INVALID_ARGUMENT;
        }
    }
    if (executable_host != NULL) {
        status = hl_c_backend_executable_open(&backend->services, executable_host, &executable);
        if (status != HL_STATUS_OK) {
            hl_options_destroy(&options);
            hl_host_linux_destroy(backend->host);
            free(backend);
            return status;
        }
        config.executable = &executable;
    }
    status = hl_engine_create_with_options(&config, &backend->services, &options, &backend->engine);
    hl_options_destroy(&options);
    if (status != HL_STATUS_OK) {
        hl_c_backend_executable_discard(&backend->services, &executable);
        if (standard_fds != NULL)
            for (index = 0; index < 3; ++index)
                (void)backend->services.file->close(backend->services.context, imported[index].value);
        hl_host_linux_destroy(backend->host);
        free(backend);
        return status;
    }
    backend->result.abi = HL_ENGINE_ABI;
    backend->result.size = sizeof(backend->result);
    *output = backend;
    return HL_STATUS_OK;
}

int32_t hl_c_backend_run(hl_c_backend *backend, int32_t argc, const char *const *argv) {
    if (backend == NULL) return HL_STATUS_INVALID_ARGUMENT;
    return hl_engine_run(backend->engine, argc, argv, &backend->result);
}

int32_t hl_c_backend_request(hl_c_backend *backend, uint32_t request, int32_t signal) {
    if (backend == NULL) return HL_STATUS_INVALID_ARGUMENT;
    if (request == HL_ENGINE_REQUEST_SIGNAL)
        return hl_engine_request(backend->engine, request, &signal, sizeof(signal));
    return hl_engine_request(backend->engine, request, NULL, 0);
}

uint32_t hl_c_backend_exit_kind(const hl_c_backend *backend) { return backend == NULL ? 0 : backend->result.kind; }
int32_t hl_c_backend_exit_status(const hl_c_backend *backend) { return backend == NULL ? -1 : backend->result.guest_status; }
uint64_t hl_c_backend_exit_detail(const hl_c_backend *backend) { return backend == NULL ? 0 : backend->result.detail; }

void hl_c_backend_destroy(hl_c_backend *backend) {
    if (backend == NULL) return;
    hl_engine_destroy(backend->engine);
    hl_host_linux_destroy(backend->host);
    free(backend);
}
