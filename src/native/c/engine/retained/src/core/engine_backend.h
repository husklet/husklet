#ifndef HL_ENGINE_BACKEND_H
#define HL_ENGINE_BACKEND_H

#include "hl/engine.h"
#include "hl/linux_abi.h"
#include "options.h"

typedef struct hl_engine_backend {
    uint32_t guest_isa;
    hl_status (*start_process)(const hl_host_services *host, hl_linux_abi *box, hl_options *options,
                               const hl_engine_config *config, uint32_t argc, const char *const argv[],
                               hl_host_handle *process, hl_host_handle *result);
    hl_status (*finish_process)(const hl_host_services *host, hl_host_handle token, const hl_host_result *waited,
                                hl_engine_exit *result);
    void (*release_process_result)(const hl_host_services *host, hl_host_handle token);
} hl_engine_backend;

void hl_engine_backend_register(const hl_engine_backend *backend);
void hl_target_register_backend(void);
void hl_target_runtime_init(void);
/* Internal launch path: atomically imports an already validated, instance-owned option snapshot. */
hl_status hl_engine_create_with_options(const hl_engine_config *config, const hl_host_services *host,
                                        const hl_options *options, hl_engine **out_engine);

#endif
