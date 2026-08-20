#ifndef HL_C_BRIDGE_API_H
#define HL_C_BRIDGE_API_H

#include "hl/base.h"
#include "hl/engine.h"
#include "hl/syscall_trap.h"
#include "main_plan.h"

#include <stddef.h>
#include <stdint.h>

typedef struct hl_c_backend hl_c_backend;

#define HL_C_BRIDGE_API_ABI 1u

typedef struct hl_c_bridge_api {
    uint32_t abi;
    uint32_t size;
    uint32_t (*engine_abi)(void);
    const char *(*engine_version)(void);
    int32_t (*leak_check_nonvacuity)(void);
    int32_t (*checkpoint_broker_pair)(int32_t *parent, int32_t *child);
    int32_t (*checkpoint_broker_accept)(int32_t broker, int32_t timeout_ms, uint64_t *host_pid);
    int32_t (*checkpoint_trigger_create)(int32_t *descriptor, void **mapping);
    uint32_t (*checkpoint_trigger_bump)(void *mapping);
    void (*checkpoint_trigger_destroy)(void *mapping, int32_t descriptor);
    int32_t (*checkpoint_adopt)(uint32_t isa, int32_t broker, int32_t trigger);
    int32_t (*checkpoint_interrupt_signal)(uint32_t isa);
    int32_t (*checkpoint_configure)(hl_c_backend *backend, int32_t broker, int32_t trigger);
    hl_status (*executable_open)(const hl_host_services *services, const char *path,
                                 hl_engine_executable *output);
    void (*executable_discard)(const hl_host_services *services, hl_engine_executable *executable);
    int32_t (*create)(uint32_t isa, const char *rootfs, const char *executable_host,
                      int32_t executable_fd, const hl_c_main_image_plan *image_plan,
                      const void *interpreter_image, size_t interpreter_size,
                      uint32_t option_count, const char *const *option_names,
                      const char *const *option_values, const int32_t standard_fds[3],
                      int32_t provider_fd, void *syscall_context,
                      hl_syscall_trap_fn syscall_dispatch, hl_c_backend **output);
    int32_t (*run)(hl_c_backend *backend, int32_t argc, const char *const *argv);
    int32_t (*request)(hl_c_backend *backend, uint32_t request, int32_t signal);
    int32_t (*exit)(hl_c_backend *backend, hl_engine_exit *result);
    void (*destroy)(hl_c_backend *backend);
    int32_t (*checkpoint_broker_accept_authenticated)(int32_t broker, int32_t timeout_ms,
                                                       uint64_t *host_pid, uint64_t *host_birth,
                                                       uint64_t *host_generation, int32_t *process_handle);
    int32_t (*guest_pid)(const hl_c_backend *backend);
    int32_t (*process_identity_signal)(int32_t handle, uint64_t host_pid, int32_t signal);
} hl_c_bridge_api;

HL_EXTERN_C_BEGIN

HL_API const hl_c_bridge_api *hl_c_bridge_api_v1(void);

HL_API int32_t hl_c_backend_leak_check_nonvacuity(void);
/* Checkpoint transport descriptors are owned by the caller. Adoption borrows
 * and duplicates both inputs before moving the duplicates into private space.
 * Hosts without checkpoint transport return HL_STATUS_NOT_SUPPORTED from the
 * status-valued operations after clearing every output. Descriptor-valued
 * operations return -1, clear their outputs, and set errno to ENOTSUP. */
HL_API int32_t hl_c_backend_checkpoint_broker_pair(int32_t *parent, int32_t *child);
HL_API int32_t hl_c_backend_checkpoint_broker_accept(int32_t broker, int32_t timeout_ms,
                                                     uint64_t *host_pid);
HL_API int32_t hl_c_backend_checkpoint_broker_accept_authenticated(int32_t broker, int32_t timeout_ms,
                                                                   uint64_t *host_pid, uint64_t *host_birth,
                                                                   uint64_t *host_generation,
                                                                   int32_t *process_handle);
HL_API int32_t hl_c_backend_checkpoint_peer_authenticate_test(int32_t descriptor, uint64_t claimed_pid,
                                                             uint64_t *host_pid, uint64_t *host_birth);
HL_API int32_t hl_c_backend_checkpoint_channel_connect_test(int32_t broker_child);
HL_API int32_t hl_c_backend_checkpoint_process_identity_open_test(int32_t pid, uint64_t expected_birth,
                                                                 uint64_t expected_generation,
                                                                 uint64_t *actual_birth,
                                                                 uint64_t *actual_generation);
HL_API int32_t hl_c_backend_checkpoint_peer_identity_open_test(int32_t descriptor, uint64_t claimed_pid,
                                                              uint64_t *actual_pid, uint64_t *actual_birth,
                                                              uint64_t *actual_generation);
HL_API int32_t hl_c_backend_checkpoint_trigger_create(int32_t *descriptor, void **mapping);
HL_API uint32_t hl_c_backend_checkpoint_trigger_bump(void *mapping);
HL_API void hl_c_backend_checkpoint_trigger_destroy(void *mapping, int32_t descriptor);
HL_API int32_t hl_c_backend_checkpoint_adopt(uint32_t isa, int32_t broker, int32_t trigger);
HL_API int32_t hl_c_backend_checkpoint_configure(hl_c_backend *backend, int32_t broker, int32_t trigger);
HL_API int32_t hl_c_backend_checkpoint_interrupt_signal(uint32_t isa);
/* output is required and is cleared before any other input is validated; every failure leaves it NULL. */
HL_API int32_t hl_c_backend_create(uint32_t isa, const char *rootfs, const char *executable_host,
                                   int32_t executable_fd, const hl_c_main_image_plan *image_plan,
                                   const void *interpreter_image, size_t interpreter_size,
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
/* The container-namespace pid of the guest process this backend launched, or 0 before the launched
 * process has published one and after it has been reaped. A checkpoint image names each captured
 * member by exactly this number and a restore re-forks it under the same one, so it is the identity a
 * host may hold across a capture. */
HL_API int32_t hl_c_backend_guest_pid(const hl_c_backend *backend);
/* Delivers one signal to the exact process incarnation an authenticated peer capability names.
 *
 * `handle` is the capability an authenticated broker accept produced and `host_pid` the identity it
 * authenticated. Delivery is refused, not retargeted, once that incarnation is gone: the capability is
 * what makes this safe against pid reuse, which a bare kill(2) on a remembered pid is not. Signal 0
 * probes reachability without delivering. Returns 0 on delivery and -1 otherwise. */
HL_API int32_t hl_c_backend_process_identity_signal(int32_t handle, uint64_t host_pid, int32_t signal);
HL_API uint64_t hl_c_backend_translation_count(const hl_c_backend *backend);
HL_API void hl_c_backend_destroy(hl_c_backend *backend);

HL_STATIC_ASSERT(sizeof(hl_c_main_image_plan) == 48, "main image plan size ABI drifted");
HL_STATIC_ASSERT(offsetof(hl_c_main_image_plan, link_start) == 16, "main image plan link_start ABI drifted");
HL_STATIC_ASSERT(offsetof(hl_c_main_image_plan, interpreter_identity) == 40,
                 "main image plan interpreter identity ABI drifted");

HL_EXTERN_C_END

#endif
