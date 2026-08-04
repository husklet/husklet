#include "support.h"

#include "../src/dispatch/exit.h"
#include "../src/executor.h"

#include <stdio.h>
#include <string.h>

#define CHECK(expression)                                                                                              \
    do {                                                                                                               \
        if (!(expression)) {                                                                                           \
            fprintf(stderr, "exit:%d: %s\n", __LINE__, #expression);                                                \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

int main(void) {
    test_memory host = {0};
    hl_native_memory services = test_services(&host);
    hl_native_config config = test_config(&services, 0);
    hl_native_executor *executor = NULL;
    hl_native_execution execution = {0};
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    CHECK(hl_native_execution_enter(executor, &execution) == HL_NATIVE_OK);
    CHECK(hl_native_execution_exit(&execution, &output, HL_NATIVE_EXIT_SYSCALL, HL_NATIVE_ACCESS_UNKNOWN,
                                   0x4000, 0x4004, 0, 0) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_SYSCALL && output.instruction == 0x4000 && output.next == 0x4004);
    CHECK(hl_native_execution_leave(&execution) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);

    memset(&output, 0xa5, sizeof(output));
    output.abi = HL_NATIVE_ABI;
    output.size = sizeof(output);
    CHECK(hl_native_exit_build(&output, HL_NATIVE_EXIT_FAULT, HL_NATIVE_ACCESS_WRITE,
                               0x5000, 0x5000, 0x7000, 2) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FAULT && output.access == HL_NATIVE_ACCESS_WRITE);
    CHECK(output.address == 0x7000 && output.code == 2);

    output.abi = HL_NATIVE_ABI;
    output.size = sizeof(output);
    CHECK(hl_native_execution_enter(executor, &execution) == HL_NATIVE_OK);
    CHECK(hl_native_execution_exit(&execution, &output, HL_NATIVE_EXIT_FAULT, HL_NATIVE_ACCESS_UNKNOWN,
                                   0x5000, 0x5000, 0x7000, 2) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    CHECK(hl_native_exit_build(&output, HL_NATIVE_EXIT_BRANCH, HL_NATIVE_ACCESS_UNKNOWN,
                               0x5000, 0x6000, 1, 0) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_exit_build(&output, HL_NATIVE_EXIT_FALLBACK, HL_NATIVE_ACCESS_EXECUTE,
                               0x4444, 0x6000, 0x6000, 0) == HL_NATIVE_OK);
    CHECK(output.instruction == 0x4444 && output.next == 0x6000 && output.address == 0x6000);
    CHECK(hl_native_exit_build(&output, HL_NATIVE_EXIT_FALLBACK, HL_NATIVE_ACCESS_READ,
                               0x4444, 0x6000, 0x6000, 0) == HL_NATIVE_ARGUMENT);
    CHECK(hl_native_exit_build(&output, HL_NATIVE_EXIT_FATAL, HL_NATIVE_ACCESS_UNKNOWN,
                               0, 0, 0, HL_NATIVE_PLATFORM) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_FATAL && output.code == HL_NATIVE_PLATFORM);
    CHECK(hl_native_exit_build(&output, 99, HL_NATIVE_ACCESS_UNKNOWN, 0, 0, 0, 0) == HL_NATIVE_ARGUMENT);
    hl_native_destroy(executor);
    CHECK(host.repair_calls == 2 && host.release_calls == 1);
    return 0;
}
