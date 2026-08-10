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

#define g_go_image HL_TARGET_LOCAL(g_go_image)
#define g_noexit HL_TARGET_LOCAL(g_noexit)
#define g_rwx_guest HL_TARGET_LOCAL(g_rwx_guest)
#define g_stack_hi HL_TARGET_LOCAL(g_stack_hi)
#define g_stack_lo HL_TARGET_LOCAL(g_stack_lo)
#define hl_engine_entry HL_TARGET_LOCAL(engine_entry)
#define hl_run_linux_guest HL_TARGET_LOCAL(run_linux_guest)
#define hl_run_linux_guest_status HL_TARGET_LOCAL(run_linux_guest_status)
#define hl_target_register_backend HL_TARGET_LOCAL(target_register_backend)
#define hl_target_runtime_init HL_TARGET_LOCAL(target_runtime_init)
#define hl_engine_child_result_after_fork HL_TARGET_LOCAL(engine_child_result_after_fork)
#define hl_engine_child_result_publish HL_TARGET_LOCAL(engine_child_result_publish)
#define hl_engine_child_result_publish_signal HL_TARGET_LOCAL(engine_child_result_publish_signal)

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
