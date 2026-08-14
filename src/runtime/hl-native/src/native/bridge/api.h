#ifndef HL_C_BRIDGE_API_H
#define HL_C_BRIDGE_API_H

#include "hl/base.h"
#include "hl/engine.h"
#include "hl/syscall_trap.h"
#include "main_plan.h"

#include <stddef.h>
#include <stdint.h>

typedef struct hl_c_backend hl_c_backend;

HL_EXTERN_C_BEGIN

HL_API int32_t hl_c_backend_leak_check_nonvacuity(void);
/* Checkpoint transport descriptors are owned by the caller. Adoption borrows
 * and duplicates both inputs before moving the duplicates into private space. */
HL_API int32_t hl_c_backend_checkpoint_broker_pair(int32_t *parent, int32_t *child);
HL_API int32_t hl_c_backend_checkpoint_broker_accept(int32_t broker, int32_t timeout_ms,
                                                     uint64_t *host_pid);
HL_API int32_t hl_c_backend_checkpoint_trigger_create(int32_t *descriptor, void **mapping);
HL_API uint32_t hl_c_backend_checkpoint_trigger_bump(void *mapping);
HL_API void hl_c_backend_checkpoint_trigger_destroy(void *mapping, int32_t descriptor);
HL_API int32_t hl_c_backend_checkpoint_adopt(uint32_t isa, int32_t broker, int32_t trigger);
HL_API int32_t hl_c_backend_checkpoint_configure(hl_c_backend *backend, int32_t broker, int32_t trigger);
HL_API int32_t hl_c_backend_checkpoint_interrupt_signal(uint32_t isa);
/* output is required and is cleared before any other input is validated; every failure leaves it NULL. */
HL_API int32_t hl_c_backend_create(uint32_t isa, const char *rootfs, const char *executable_host,
                                   int32_t executable_fd, const hl_c_main_image_plan *image_plan,
                                   uint32_t option_count, const char *const *option_names,
                                   const char *const *option_values, const int32_t standard_fds[3],
                                   int32_t provider_fd, void *syscall_context,
                                   hl_syscall_trap_fn syscall_dispatch, hl_c_backend **output);
HL_API int32_t hl_c_backend_run(hl_c_backend *backend, int32_t argc, const char *const *argv);
HL_API int32_t hl_c_backend_request(hl_c_backend *backend, uint32_t request, int32_t signal);
/* Copies one coherently published exit record. While run is active this returns
 * the last complete record, never fields from the record run is constructing. */
HL_API int32_t hl_c_backend_exit(hl_c_backend *backend, hl_engine_exit *result);
/* Compatibility accessors retained for previously linked bridge consumers.
 * Each returns one synchronized field; callers needing a coherent tuple use
 * hl_c_backend_exit. */
HL_API uint32_t hl_c_backend_exit_kind(const hl_c_backend *backend);
HL_API int32_t hl_c_backend_exit_status(const hl_c_backend *backend);
HL_API uint64_t hl_c_backend_exit_detail(const hl_c_backend *backend);
HL_API uint64_t hl_c_backend_translation_count(const hl_c_backend *backend);
HL_API void hl_c_backend_destroy(hl_c_backend *backend);

HL_STATIC_ASSERT(sizeof(hl_c_main_image_plan) == 48, "main image plan size ABI drifted");
HL_STATIC_ASSERT(offsetof(hl_c_main_image_plan, link_start) == 16, "main image plan link_start ABI drifted");
HL_STATIC_ASSERT(offsetof(hl_c_main_image_plan, interpreter_identity) == 40,
                 "main image plan interpreter identity ABI drifted");

HL_EXTERN_C_END

#endif
