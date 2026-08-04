#if defined(__linux__)
#define _XOPEN_SOURCE 700
#endif

#include "../../include/executor.h"

#include <stdint.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>

#define HL_NATIVE_ALTSTACK_SIZE (512u << 10)

#if defined(__GNUC__) || defined(__clang__)
#define HL_NATIVE_NO_STACK_PROTECTOR __attribute__((no_stack_protector))
#else
#define HL_NATIVE_NO_STACK_PROTECTOR
#endif

typedef struct hl_native_fault_thread_state {
    uint64_t generation;
    uint32_t attached;
    volatile sig_atomic_t active;
    volatile sig_atomic_t recursive;
    hl_native_fault_scope scope;
#if defined(__linux__)
    stack_t prior;
    void *storage;
#endif
} hl_native_fault_thread_state;

#if defined(__linux__)
/* initial-exec is part of this boundary: PIC general-dynamic TLS may call
 * __tls_get_addr and is therefore forbidden in the signal-facing path. */
static _Thread_local __attribute__((tls_model("initial-exec")))
    hl_native_fault_thread_state hl_native_fault_thread;
#else
static _Thread_local hl_native_fault_thread_state hl_native_fault_thread;
#endif

#if defined(HL_NATIVE_THREAD_TEST)
void hl_native_fault_thread_test_recursion(int active) {
    hl_native_fault_thread.recursive = active != 0;
}
#endif

static uint64_t next_generation(uint64_t generation) {
    generation++;
    return generation == 0 ? 1 : generation;
}

hl_native_status hl_native_fault_thread_attach(void) {
    hl_native_fault_thread_state *state = &hl_native_fault_thread;
    if (state->attached) return HL_NATIVE_STATE;
#if defined(__linux__)
    stack_t prior;
    if (sigaltstack(NULL, &prior) != 0 || (prior.ss_flags & SS_ONSTACK) != 0)
        return HL_NATIVE_PLATFORM;
    void *storage = malloc(HL_NATIVE_ALTSTACK_SIZE);
    if (storage == NULL) return HL_NATIVE_MEMORY;
    stack_t installed = {.ss_sp = storage, .ss_size = HL_NATIVE_ALTSTACK_SIZE, .ss_flags = 0};
    if (sigaltstack(&installed, NULL) != 0) {
        free(storage);
        return HL_NATIVE_PLATFORM;
    }
    state->prior = prior;
    state->storage = storage;
    state->generation = next_generation(state->generation);
    state->attached = 1;
    return HL_NATIVE_OK;
#else
    return HL_NATIVE_PLATFORM;
#endif
}

static int same_scope(const hl_native_fault_scope *left,
                      const hl_native_fault_scope *right) {
    return left->abi == right->abi && left->size == right->size &&
           left->architecture == right->architecture &&
           left->reserved == right->reserved && left->executor == right->executor &&
           left->cpu == right->cpu;
}

hl_native_status hl_native_fault_thread_publish(const hl_native_fault_scope *scope,
                                                 uint64_t *generation) {
    hl_native_fault_thread_state *state = &hl_native_fault_thread;
    if (scope == NULL || generation == NULL || scope->abi != HL_NATIVE_ABI ||
        scope->size != sizeof(*scope) || scope->reserved != 1 ||
        scope->executor == NULL || scope->cpu == NULL)
        return HL_NATIVE_ARGUMENT;
    if (!state->attached || state->active || state->recursive) return HL_NATIVE_STATE;
    state->scope = *scope;
    state->generation = next_generation(state->generation);
    atomic_signal_fence(memory_order_release);
    state->active = 1;
    *generation = state->generation;
    return HL_NATIVE_OK;
}

hl_native_status hl_native_fault_thread_unpublish(const hl_native_fault_scope *scope,
                                                   uint64_t generation) {
    hl_native_fault_thread_state *state = &hl_native_fault_thread;
    if (scope == NULL || generation == 0) return HL_NATIVE_ARGUMENT;
    if (!state->attached || !state->active || state->recursive ||
        state->generation != generation || !same_scope(&state->scope, scope))
        return HL_NATIVE_STATE;
    state->active = 0;
    atomic_signal_fence(memory_order_acquire);
    memset(&state->scope, 0, sizeof(state->scope));
    return HL_NATIVE_OK;
}

HL_NATIVE_NO_STACK_PROTECTOR int hl_native_fault_thread_prepare(
    uint64_t host_pc, uint64_t host_address, void *host_context) {
    hl_native_fault_thread_state *state = &hl_native_fault_thread;
    if (!state->attached || !state->active || state->recursive || host_context == NULL)
        return 0;
    state->recursive = 1;
    atomic_signal_fence(memory_order_acquire);
    hl_native_fault_scope scope = state->scope;
    int prepared = hl_native_fault_scope_contains(&scope, host_pc) &&
                   hl_native_fault_scope_prepare_return(&scope, host_pc, host_address,
                                                        host_context);
    atomic_signal_fence(memory_order_release);
    state->recursive = 0;
    return prepared;
}

hl_native_status hl_native_fault_thread_after_fork_child(void) {
    hl_native_fault_thread_state *state = &hl_native_fault_thread;
    state->active = 0;
    state->recursive = 0;
    memset(&state->scope, 0, sizeof(state->scope));
    state->generation = next_generation(state->generation);
    if (!state->attached) return HL_NATIVE_OK;
#if defined(__linux__)
    stack_t installed = {
        .ss_sp = state->storage,
        .ss_size = HL_NATIVE_ALTSTACK_SIZE,
        .ss_flags = 0,
    };
    return sigaltstack(&installed, NULL) == 0 ? HL_NATIVE_OK : HL_NATIVE_PLATFORM;
#else
    return HL_NATIVE_PLATFORM;
#endif
}

hl_native_status hl_native_fault_thread_detach(void) {
    hl_native_fault_thread_state *state = &hl_native_fault_thread;
    if (!state->attached) return HL_NATIVE_STATE;
    if (state->active || state->recursive) return HL_NATIVE_STATE;
#if defined(__linux__)
    stack_t current;
    if (sigaltstack(NULL, &current) != 0 || current.ss_sp != state->storage ||
        current.ss_size != HL_NATIVE_ALTSTACK_SIZE || current.ss_flags != 0)
        return HL_NATIVE_STATE;
    if (sigaltstack(&state->prior, NULL) != 0) return HL_NATIVE_PLATFORM;
    free(state->storage);
    state->storage = NULL;
    memset(&state->prior, 0, sizeof(state->prior));
#else
    return HL_NATIVE_PLATFORM;
#endif
    state->attached = 0;
    state->generation = next_generation(state->generation);
    return HL_NATIVE_OK;
}
