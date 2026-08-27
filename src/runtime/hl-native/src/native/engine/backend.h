#ifndef HL_ENGINE_BACKEND_H
#define HL_ENGINE_BACKEND_H

#include "hl/engine.h"
#include "hl/linux_abi.h"
#include "options.h"
#include "hl/syscall_trap.h"

typedef struct hl_engine_backend {
    uint32_t guest_isa;
    hl_status (*start_process)(const hl_host_services *host, hl_linux_abi *box, hl_options *options,
                               const hl_engine_config *config, uint32_t argc, const char *const argv[],
                               void *syscall_context, hl_syscall_trap_fn syscall_dispatch, int checkpoint_broker,
                               int checkpoint_trigger, int checkpoint_control, const void *interpreter_image,
                               size_t interpreter_size, hl_host_handle *process, hl_host_handle *result);
    hl_status (*finish_process)(const hl_host_services *host, hl_host_handle token, const hl_host_result *waited,
                                hl_engine_exit *result, uint64_t *translations);
    void (*release_process_result)(const hl_host_services *host, hl_host_handle token);
    /* The container-namespace pid of the launched guest process, or 0 before the child has published
       one. Readable for as long as the process-result token lives. */
    int32_t (*process_guest_pid)(hl_host_handle token);
} hl_engine_backend;

void hl_engine_backend_register(const hl_engine_backend *backend);
void hl_target_register_backend(void);
void hl_target_runtime_init(void);
size_t hl_target_backend_tree_shared_size(int enabled);
void hl_target_backend_tree_child_begin(void *shared, size_t shared_size);
void hl_target_backend_tree_reap_report(void *shared, size_t shared_size, hl_linux_abi *box);
/* Internal launch path: atomically imports an already validated, instance-owned option snapshot. */
hl_status hl_engine_create_with_options(const hl_engine_config *config, const hl_host_services *host,
                                        const hl_options *options, hl_engine **out_engine);
/* Product-only path. The caller keeps options alive through engine destruction.
 * Execution workers inherit a process-private copy and may update runtime state there. */
hl_status hl_engine_create_with_borrowed_options(const hl_engine_config *config, const hl_host_services *host,
                                                 const hl_options *options, hl_engine **out_engine);
hl_status hl_engine_create_with_borrowed_options_and_syscall_trap(const hl_engine_config *config,
                                                                  const hl_host_services *host,
                                                                  const hl_options *options, void *syscall_context,
                                                                  hl_syscall_trap_fn syscall_dispatch,
                                                                  hl_engine **out_engine);
hl_status hl_engine_create_with_borrowed_options_and_syscall_trap_and_interpreter(
    const hl_engine_config *config, const hl_host_services *host, const hl_options *options, void *syscall_context,
    hl_syscall_trap_fn syscall_dispatch, const void *interpreter_image, size_t interpreter_size,
    hl_engine **out_engine);
uint64_t hl_engine_translation_count(const hl_engine *engine);
/* The container-namespace pid of the guest process this engine launched, or 0 while no launched
   process has published one. Stable for the run: the value is the identity a checkpoint image names
   the process by and a restore re-forks it under. */
int32_t hl_engine_guest_pid(hl_engine *engine);
hl_status hl_engine_checkpoint_configure(hl_engine *engine, int broker, int trigger);
void hl_engine_checkpoint_fork_prepare(void);
void hl_engine_checkpoint_fork_parent(void);
void hl_engine_checkpoint_fork_child(int broker, int trigger, int control);
int hl_engine_checkpoint_descriptors_register(int first, int second);
/* Re-arms the engine-owner lifetime capability after fork removed every peer thread. */
int hl_engine_checkpoint_lifetime_after_fork(void);

#if defined(HL_NATIVE_TEST_HOOKS)
uint32_t hl_engine_finish_test_arm(hl_engine *engine);
uint32_t hl_engine_finish_test_phase(hl_engine *engine);
void hl_engine_finish_test_release(hl_engine *engine);
#endif

#endif
