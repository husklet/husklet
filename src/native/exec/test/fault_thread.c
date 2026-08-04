#define _XOPEN_SOURCE 700
#define HL_NATIVE_THREAD_TEST 1

#include "../src/fault/thread.c"

#include <pthread.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(value) do { if (!(value)) abort(); } while (0)

int hl_native_fault_scope_contains(const hl_native_fault_scope *scope, uint64_t pc) {
    (void)scope;
    return pc == 0x1234;
}

int hl_native_fault_scope_prepare_return(const hl_native_fault_scope *scope, uint64_t pc,
                                         uint64_t address, void *context) {
    (void)scope;
    return pc == 0x1234 && address == 0x5678 && context != NULL;
}

static hl_native_fault_scope scope_for(uintptr_t identity) {
    hl_native_fault_scope scope = {
        .abi = HL_NATIVE_ABI,
        .size = sizeof(scope),
        .architecture = HL_NATIVE_AARCH64,
        .reserved = 1,
        .executor = (hl_native_executor *)identity,
        .cpu = (hl_native_cpu *)(identity + 1),
    };
    return scope;
}

static void *isolated(void *opaque) {
    uintptr_t identity = (uintptr_t)opaque;
    hl_native_fault_scope scope = scope_for(identity);
    uint64_t generation;
    CHECK(hl_native_fault_thread_attach() == HL_NATIVE_OK);
    CHECK(hl_native_fault_thread_publish(&scope, &generation) == HL_NATIVE_OK);
    CHECK(hl_native_fault_thread_unpublish(&scope, generation) == HL_NATIVE_OK);
    CHECK(hl_native_fault_thread_detach() == HL_NATIVE_OK);
    return NULL;
}

int main(void) {
    void *prior_storage = malloc(64u << 10);
    CHECK(prior_storage != NULL);
    stack_t prior = {.ss_sp = prior_storage, .ss_size = 64u << 10, .ss_flags = 0};
    CHECK(sigaltstack(&prior, NULL) == 0);

    hl_native_fault_scope first = scope_for(2);
    hl_native_fault_scope second = scope_for(4);
    uint64_t generation;
    CHECK(hl_native_fault_thread_attach() == HL_NATIVE_OK);
    CHECK(hl_native_fault_thread_attach() == HL_NATIVE_STATE);
    stack_t current;
    CHECK(sigaltstack(NULL, &current) == 0 && current.ss_sp != prior.ss_sp);
    CHECK(hl_native_fault_thread_publish(&first, &generation) == HL_NATIVE_OK);
    uint64_t nested;
    CHECK(hl_native_fault_thread_publish(&second, &nested) == HL_NATIVE_STATE);
    CHECK(hl_native_fault_thread_unpublish(&first, generation + 1) == HL_NATIVE_STATE);
    CHECK(hl_native_fault_thread_unpublish(&second, generation) == HL_NATIVE_STATE);
    CHECK(hl_native_fault_thread_detach() == HL_NATIVE_STATE);
    hl_native_fault_thread_test_recursion(1);
    CHECK(!hl_native_fault_thread_prepare(1, 2, &current));
    hl_native_fault_thread_test_recursion(0);
    CHECK(hl_native_fault_thread_prepare(0x1234, 0x5678, &current));

    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        CHECK(hl_native_fault_thread_after_fork_child() == HL_NATIVE_OK);
        CHECK(hl_native_fault_thread_unpublish(&first, generation) == HL_NATIVE_STATE);
        uint64_t child_generation;
        CHECK(hl_native_fault_thread_publish(&second, &child_generation) == HL_NATIVE_OK);
        CHECK(child_generation != generation);
        CHECK(hl_native_fault_thread_unpublish(&second, child_generation) == HL_NATIVE_OK);
        CHECK(hl_native_fault_thread_detach() == HL_NATIVE_OK);
        _exit(0);
    }
    int status;
    CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    hl_native_fault_scope equal_copy = first;
    CHECK(hl_native_fault_thread_unpublish(&equal_copy, generation) == HL_NATIVE_OK);
    uint64_t retired = generation;
    CHECK(hl_native_fault_thread_publish(&first, &generation) == HL_NATIVE_OK);
    CHECK(generation != retired);
    CHECK(hl_native_fault_thread_unpublish(&first, retired) == HL_NATIVE_STATE);
    CHECK(hl_native_fault_thread_unpublish(&first, generation) == HL_NATIVE_OK);
    CHECK(hl_native_fault_thread_detach() == HL_NATIVE_OK);
    CHECK(sigaltstack(NULL, &current) == 0 && current.ss_sp == prior.ss_sp &&
          current.ss_size == prior.ss_size && current.ss_flags == prior.ss_flags);

    pthread_t threads[2];
    CHECK(pthread_create(&threads[0], NULL, isolated, (void *)8) == 0);
    CHECK(pthread_create(&threads[1], NULL, isolated, (void *)16) == 0);
    CHECK(pthread_join(threads[0], NULL) == 0);
    CHECK(pthread_join(threads[1], NULL) == 0);
    for (unsigned iteration = 0; iteration < 32; ++iteration) {
        CHECK(hl_native_fault_thread_attach() == HL_NATIVE_OK);
        CHECK(hl_native_fault_thread_detach() == HL_NATIVE_OK);
    }

    CHECK(hl_native_fault_thread_attach() == HL_NATIVE_OK);
    stack_t owned;
    CHECK(sigaltstack(NULL, &owned) == 0);
    void *foreign_storage = malloc(64u << 10);
    CHECK(foreign_storage != NULL);
    stack_t foreign = {.ss_sp = foreign_storage, .ss_size = 64u << 10, .ss_flags = 0};
    CHECK(sigaltstack(&foreign, NULL) == 0);
    CHECK(hl_native_fault_thread_detach() == HL_NATIVE_STATE);
    CHECK(sigaltstack(&owned, NULL) == 0);
    CHECK(hl_native_fault_thread_detach() == HL_NATIVE_OK);
    free(foreign_storage);
    stack_t disabled = {.ss_flags = SS_DISABLE};
    CHECK(sigaltstack(&disabled, NULL) == 0);
    free(prior_storage);
    return 0;
}
