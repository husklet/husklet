#ifndef HL_CORE_TARGET_NAMESPACE_H
#define HL_CORE_TARGET_NAMESPACE_H

/*
 * Production translators are unity translation units and therefore carry a
 * small set of process-global JIT/Linux-ABI symbols.  An embedding build sets
 * HL_TARGET_NAMESPACE to the guest ISA token so both translators can coexist
 * in one image.  Standalone runners leave it unset and retain their historical
 * symbol names.
 */
#ifdef HL_TARGET_NAMESPACE
#define HL_TARGET_JOIN_INNER(a, b) a##b
#define HL_TARGET_JOIN(a, b) HL_TARGET_JOIN_INNER(a, b)
#define HL_TARGET_SYMBOL_INNER(ns, name) hl_##ns##_##name
#define HL_TARGET_SYMBOL(ns, name) HL_TARGET_SYMBOL_INNER(ns, name)
#define HL_TARGET_LOCAL(name) HL_TARGET_SYMBOL(HL_TARGET_NAMESPACE, name)

#define g_rwx_guest HL_TARGET_LOCAL(g_rwx_guest)
#define g_stack_hi HL_TARGET_LOCAL(g_stack_hi)
#define g_stack_lo HL_TARGET_LOCAL(g_stack_lo)
#define hl_run_linux_guest HL_TARGET_LOCAL(run_linux_guest)
#define hl_run_linux_guest_status HL_TARGET_LOCAL(run_linux_guest_status)
#define hl_run_linux_guest_translations HL_TARGET_LOCAL(run_linux_guest_translations)
#define hl_target_register_backend HL_TARGET_LOCAL(target_register_backend)
#define hl_target_runtime_init HL_TARGET_LOCAL(target_runtime_init)
#define hl_target_syscall_trap_install HL_TARGET_LOCAL(target_syscall_trap_install)
#define hl_engine_child_result_after_fork HL_TARGET_LOCAL(engine_child_result_after_fork)
#define hl_engine_child_result_publish HL_TARGET_LOCAL(engine_child_result_publish)
#define hl_engine_child_result_publish_signal HL_TARGET_LOCAL(engine_child_result_publish_signal)
#define hl_engine_child_result_publish_guest_pid HL_TARGET_LOCAL(engine_child_result_publish_guest_pid)
#define hl_engine_checkpoint_lifetime_after_fork HL_TARGET_LOCAL(engine_checkpoint_lifetime_after_fork)

/* Each embedded translator carries checkpoint channel state consumed by its
 * Linux-ABI unity unit. Keep adoption and execution in the same ISA namespace. */
#define hl_ckpt_channel_adopt HL_TARGET_LOCAL(ckpt_channel_adopt)
#define hl_ckpt_channel_acquire HL_TARGET_LOCAL(ckpt_channel_acquire)
#define hl_ckpt_channel_forget_for_test HL_TARGET_LOCAL(ckpt_channel_forget_for_test)
#define hl_ckpt_channel_current_for_test HL_TARGET_LOCAL(ckpt_channel_current_for_test)
#define hl_ckpt_channel_authenticate_peer HL_TARGET_LOCAL(ckpt_channel_authenticate_peer)
#define hl_ckpt_channel_test_claimed_pid HL_TARGET_LOCAL(ckpt_channel_test_claimed_pid)
#define hl_ckpt_channel_call HL_TARGET_LOCAL(ckpt_channel_call)
#define hl_ckpt_channel_notify HL_TARGET_LOCAL(ckpt_channel_notify)
#define hl_ckpt_channel_call_receive_descriptor HL_TARGET_LOCAL(ckpt_channel_call_receive_descriptor)
#define hl_ckpt_channel_broker HL_TARGET_LOCAL(ckpt_channel_broker)
#define hl_ckpt_channel_failure HL_TARGET_LOCAL(ckpt_channel_failure)
#define hl_ckpt_channel_owns_descriptor HL_TARGET_LOCAL(ckpt_channel_owns_descriptor)
#define hl_ckpt_channel_publish HL_TARGET_LOCAL(ckpt_channel_publish)
#define hl_ckpt_trigger_descriptor HL_TARGET_LOCAL(ckpt_trigger_descriptor)
#define hl_ckpt_trigger_publish HL_TARGET_LOCAL(ckpt_trigger_publish)
#define hl_ckpt_broker_pair HL_TARGET_LOCAL(ckpt_broker_pair)
#define hl_ckpt_broker_accept HL_TARGET_LOCAL(ckpt_broker_accept)
#define hl_ckpt_trigger_create HL_TARGET_LOCAL(ckpt_trigger_create)
#define hl_ckpt_trigger_bump HL_TARGET_LOCAL(ckpt_trigger_bump)
#define hl_ckpt_trigger_destroy HL_TARGET_LOCAL(ckpt_trigger_destroy)
#define hl_ckpt_interrupt_signal HL_TARGET_LOCAL(ckpt_interrupt_signal)
#define hl_ckpt_interrupt_executors HL_TARGET_LOCAL(ckpt_interrupt_executors)
#define ckpt_request_generation HL_TARGET_LOCAL(ckpt_request_generation)
#define hl_checkpoint_restore_claim_test HL_TARGET_LOCAL(checkpoint_restore_claim_test)
#define hl_checkpoint_restore_slice_test HL_TARGET_LOCAL(checkpoint_restore_slice_test)
#define hl_checkpoint_gmap_release_test HL_TARGET_LOCAL(checkpoint_gmap_release_test)

#define hl_linux_bus_active HL_TARGET_LOCAL(linux_bus_active)
#define hl_linux_bus_fault HL_TARGET_LOCAL(linux_bus_fault)
#define hl_linux_bus_generation HL_TARGET_LOCAL(linux_bus_generation)
#define hl_linux_bus_hit HL_TARGET_LOCAL(linux_bus_hit)
#define hl_linux_bus_set_change_callback HL_TARGET_LOCAL(linux_bus_set_change_callback)
#define hl_linux_bus_set_transition_callbacks HL_TARGET_LOCAL(linux_bus_set_transition_callbacks)
#define hl_linux_bus_transition_add HL_TARGET_LOCAL(linux_bus_transition_add)
#define hl_linux_bus_transition_begin HL_TARGET_LOCAL(linux_bus_transition_begin)
#define hl_linux_bus_transition_clear HL_TARGET_LOCAL(linux_bus_transition_clear)
#define hl_linux_bus_transition_end HL_TARGET_LOCAL(linux_bus_transition_end)

#define jit_cache_diag HL_TARGET_LOCAL(jit_cache_diag)
#define jit_guest_bus_active HL_TARGET_LOCAL(jit_guest_bus_active)
#define jit_guest_bus_arm_latched HL_TARGET_LOCAL(jit_guest_bus_arm_latched)
#define jit_guest_bus_bind HL_TARGET_LOCAL(jit_guest_bus_bind)
#define jit_guest_bus_changed HL_TARGET_LOCAL(jit_guest_bus_changed)
#define jit_guest_bus_fault HL_TARGET_LOCAL(jit_guest_bus_fault)
#define jit_guest_bus_transition_begin HL_TARGET_LOCAL(jit_guest_bus_transition_begin)
#define jit_guest_bus_transition_end HL_TARGET_LOCAL(jit_guest_bus_transition_end)
#define jit_hostpc_alias_kind HL_TARGET_LOCAL(jit_hostpc_alias_kind)
#define jit_hostpc_lookup HL_TARGET_LOCAL(jit_hostpc_lookup)
#define jit_pc_in_cache HL_TARGET_LOCAL(jit_pc_in_cache)
#define jit_pc_in_retained_cache HL_TARGET_LOCAL(jit_pc_in_retained_cache)

/*
 * Checkpoint SCM image helpers (src/linux_abi/syscall/{event,inotify}.c).
 * These are non-static and declared in src/linux_abi/checkpoint.h; without a
 * rename both unity translators export the same four symbols and the dual
 * embedded archive fails to link with "multiple definition".  namespace.h is
 * the first include of every unity TU, so the rename covers both the
 * declaration in checkpoint.h and the definitions.
 */
#define epoll_scm_image_export HL_TARGET_LOCAL(epoll_scm_image_export)
#define epoll_scm_image_import HL_TARGET_LOCAL(epoll_scm_image_import)
#define typed_inotify_scm_image_export HL_TARGET_LOCAL(typed_inotify_scm_image_export)
#define typed_inotify_scm_image_import HL_TARGET_LOCAL(typed_inotify_scm_image_import)
#endif

#endif
