#include "support.h"

#include "../src/executor.h"

#include <pthread.h>
#include <stdio.h>

#define CHECK(expression)                                                                                              \
    do {                                                                                                               \
        if (!(expression)) {                                                                                           \
            fprintf(stderr, "gate:%d: %s\n", __LINE__, #expression);                                                \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

enum { GENERATION_THREADS = 4, GENERATIONS_PER_THREAD = 64 };

typedef struct generation_worker {
    hl_native_executor *executor;
    uint64_t values[GENERATIONS_PER_THREAD];
    int failed;
} generation_worker;

static void *issue_generations(void *opaque) {
    generation_worker *worker = opaque;
    for (size_t index = 0; index < GENERATIONS_PER_THREAD; ++index) {
        hl_native_execution execution = {0};
        if (hl_native_execution_enter(worker->executor, &execution) != HL_NATIVE_OK) {
            worker->failed = 1;
            return NULL;
        }
        worker->values[index] = hl_native_execution_generation(&execution);
        if (worker->values[index] == 0 || hl_native_execution_leave(&execution) != HL_NATIVE_OK) {
            worker->failed = 1;
            return NULL;
        }
    }
    return NULL;
}

int main(void) {
    test_memory host = {0};
    hl_native_memory services = test_services(&host);
    hl_native_config config = test_config(&services, 0);
    hl_native_executor *executor = NULL;
    hl_native_execution first = {0}, second = {0};
    hl_native_aarch64_cpu aarch64 = {0};
    hl_native_cpu cpu = {
        .abi = HL_NATIVE_ABI,
        .size = sizeof(cpu),
        .architecture = HL_NATIVE_AARCH64,
        .state.aarch64 = &aarch64,
    };
    hl_native_fault_scope scope = {0};
    hl_native_change replace = {
        .abi = HL_NATIVE_ABI,
        .size = sizeof(replace),
        .kind = HL_NATIVE_REPLACE,
        .mapping_epoch = 3,
    };

    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    CHECK(hl_native_execution_enter(executor, &first) == HL_NATIVE_OK);
    uint64_t first_generation = hl_native_execution_generation(&first);
    CHECK(first_generation != 0);
    aarch64.certificate_token = UINT64_MAX;
    CHECK(hl_native_execution_bind_certificate(&first, &aarch64.certificate_token) == HL_NATIVE_OK);
    CHECK(aarch64.certificate_token == 0);
    aarch64.certificate_token = first_generation;
    CHECK(hl_native_execution_enter(executor, &first) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_execution_enter(executor, &second) == HL_NATIVE_OK);
    CHECK(hl_native_execution_generation(&second) != 0);
    CHECK(hl_native_execution_generation(&second) != first_generation);
    CHECK(hl_native_executor_gate_enter(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_STATE);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_execution_leave(&first) == HL_NATIVE_OK);
    CHECK(aarch64.certificate_token == 0);
    CHECK(hl_native_execution_generation(&first) == 0);
    CHECK(hl_native_execution_leave(&first) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_execution_enter(executor, &first) == HL_NATIVE_OK);
    CHECK(hl_native_execution_generation(&first) != 0);
    CHECK(hl_native_execution_generation(&first) != first_generation);
    CHECK(hl_native_execution_leave(&first) == HL_NATIVE_OK);
    CHECK(hl_native_executor_gate_enter(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_execution_leave(&second) == HL_NATIVE_OK);
    CHECK(hl_native_executor_gate_enter(executor) == HL_NATIVE_OK);
    CHECK(hl_native_execution_enter(executor, &first) == HL_NATIVE_STATE);
    CHECK(hl_native_executor_gate_enter(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_STATE);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_after_fork(executor, 0) == HL_NATIVE_STATE);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_STATE);
    CHECK(host.begin_calls == 0 && host.repair_calls == 0);
    hl_native_executor_gate_leave(executor);

    generation_worker workers[GENERATION_THREADS] = {0};
    pthread_t threads[GENERATION_THREADS];
    for (size_t index = 0; index < GENERATION_THREADS; ++index) {
        workers[index].executor = executor;
        CHECK(pthread_create(&threads[index], NULL, issue_generations, &workers[index]) == 0);
    }
    for (size_t index = 0; index < GENERATION_THREADS; ++index)
        CHECK(pthread_join(threads[index], NULL) == 0 && !workers[index].failed);
    for (size_t left_thread = 0; left_thread < GENERATION_THREADS; ++left_thread)
        for (size_t left = 0; left < GENERATIONS_PER_THREAD; ++left)
            for (size_t right_thread = left_thread; right_thread < GENERATION_THREADS; ++right_thread)
                for (size_t right = right_thread == left_thread ? left + 1 : 0;
                     right < GENERATIONS_PER_THREAD; ++right)
                    CHECK(workers[left_thread].values[left] != workers[right_thread].values[right]);

    CHECK(hl_native_fault_scope_enter(executor, &cpu, &scope) == HL_NATIVE_OK);
    CHECK(scope.executor == executor && scope.cpu == &cpu);
    CHECK(!hl_native_fault_scope_contains(&scope, UINT64_C(0x1234)));
    hl_native_provenance provenance;
    CHECK(!hl_native_fault_scope_provenance(&scope, UINT64_C(0x1234), &provenance));
    CHECK(!hl_native_fault_scope_prepare_return(&scope, UINT64_C(0x1234),
                                                UINT64_C(0x5678), &provenance));
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_fault_scope_leave(&scope) == HL_NATIVE_OK);
    CHECK(hl_native_fault_scope_leave(&scope) == HL_NATIVE_ARGUMENT);

    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_STATE);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    CHECK(host.repair_calls == 1);
    atomic_store_explicit(&executor->activation_generation, UINT64_MAX - 1,
                          memory_order_relaxed);
    CHECK(hl_native_execution_enter(executor, &first) == HL_NATIVE_OK);
    CHECK(hl_native_execution_generation(&first) == UINT64_MAX);
    CHECK(hl_native_execution_leave(&first) == HL_NATIVE_OK);
    CHECK(hl_native_execution_enter(executor, &first) == HL_NATIVE_OK);
    CHECK(hl_native_execution_generation(&first) == 0);
    CHECK(hl_native_execution_leave(&first) == HL_NATIVE_OK);
    CHECK(atomic_load_explicit(&executor->activation_generation,
                               memory_order_relaxed) == UINT64_MAX);
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_OK);
    CHECK(hl_native_executor_gate_enter(executor) == HL_NATIVE_OK);
    hl_native_executor_gate_leave(executor);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
    CHECK(host.release_calls == 1);
    return 0;
}
