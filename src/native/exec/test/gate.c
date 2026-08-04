#include "support.h"

#include "../src/executor.h"

#include <stdio.h>

#define CHECK(expression)                                                                                              \
    do {                                                                                                               \
        if (!(expression)) {                                                                                           \
            fprintf(stderr, "gate:%d: %s\n", __LINE__, #expression);                                                \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

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
    CHECK(hl_native_execution_enter(executor, &first) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_execution_enter(executor, &second) == HL_NATIVE_OK);
    CHECK(hl_native_executor_gate_enter(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_changed(executor, &replace, 1) == HL_NATIVE_STATE);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_STATE);
    CHECK(hl_native_execution_leave(&first) == HL_NATIVE_OK);
    CHECK(hl_native_execution_leave(&first) == HL_NATIVE_ARGUMENT);
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
    CHECK(hl_native_executor_gate_enter(executor) == HL_NATIVE_OK);
    hl_native_executor_gate_leave(executor);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
    CHECK(host.release_calls == 1);
    return 0;
}
