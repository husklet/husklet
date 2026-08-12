#ifndef HL_C_BRIDGE_API_H
#define HL_C_BRIDGE_API_H

#include "hl/base.h"
#include "hl/syscall_trap.h"
#include "main_plan.h"

#include <stdint.h>

typedef struct hl_c_backend hl_c_backend;

HL_EXTERN_C_BEGIN

HL_API int32_t hl_c_backend_leak_check_nonvacuity(void);
HL_API int32_t hl_c_backend_create(uint32_t isa, const char *rootfs, const char *executable_host,
                                   int32_t executable_fd, const hl_c_main_image_plan *image_plan,
                                   uint32_t option_count, const char *const *option_names,
                                   const char *const *option_values, const int32_t standard_fds[3],
                                   int32_t provider_fd, void *syscall_context,
                                   hl_syscall_trap_fn syscall_dispatch, hl_c_backend **output);
HL_API int32_t hl_c_backend_run(hl_c_backend *backend, int32_t argc, const char *const *argv);
HL_API int32_t hl_c_backend_request(hl_c_backend *backend, uint32_t request, int32_t signal);
HL_API uint32_t hl_c_backend_exit_kind(const hl_c_backend *backend);
HL_API int32_t hl_c_backend_exit_status(const hl_c_backend *backend);
HL_API uint64_t hl_c_backend_exit_detail(const hl_c_backend *backend);
HL_API uint64_t hl_c_backend_translation_count(const hl_c_backend *backend);
HL_API void hl_c_backend_destroy(hl_c_backend *backend);

HL_EXTERN_C_END

#endif
