#include "support.h"
#include "../src/executor.h"

#include <stdio.h>
#include <sys/wait.h>
#include <unistd.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "direct:%d: %s\n", __LINE__, #value); abort(); } } while (0)

static hl_native_direct_authority authority(void) {
    hl_native_direct_authority value = {
        .abi = HL_NATIVE_ABI,
        .size = sizeof(value),
        .permissions = HL_NATIVE_ACCESS_READ,
        .guest_first = UINT64_C(0x1000),
        .guest_last = UINT64_C(0x2000),
        .host_first = UINT64_C(0x100000),
        .mapping_incarnation = 7,
        .mapping_generation = 8,
        .instruction_generation = 9,
    };
    return value;
}

int main(void) {
    test_memory memory = {0};
    hl_native_memory services = test_services(&memory);
    hl_native_config config = test_config(&services, 0);
    hl_native_executor *executor = NULL;
    hl_native_direct_token *first = NULL;
    hl_native_direct_token *second = NULL;
    hl_native_direct_token *third = NULL;
    hl_native_direct_authority expected = authority();
    hl_native_direct_authority observed = {0};

    size_t authenticated_bytes = 0;
    CHECK(!hl_native_ibtc_authenticated_storage_bytes(
        SIZE_MAX / sizeof(hl_native_ibtc_authenticated_entry) + 1, &authenticated_bytes));
    CHECK(hl_native_ibtc_authenticated_storage_bytes(HL_NATIVE_IBTC_COUNT,
                                                     &authenticated_bytes));
    CHECK(authenticated_bytes == HL_NATIVE_IBTC_AUTHENTICATED_BYTES);
    hl_native_ibtc_authenticated_entry *authenticated =
        hl_native_ibtc_authenticated_storage_create();
    CHECK(authenticated != NULL && ((uintptr_t)authenticated & UINT64_C(65535)) == 0);
    for (size_t index = 0; index < HL_NATIVE_IBTC_COUNT; ++index)
        CHECK(authenticated[index].target == 0 && authenticated[index].authenticated_ingress == 0 &&
              authenticated[index].target_identity == 0 &&
              atomic_load_explicit(&authenticated[index].sequence, memory_order_relaxed) == 0);
    authenticated[7].target = UINT64_C(0xaaaa);
    authenticated[7].authenticated_ingress = UINT64_C(0xbbbb);
    authenticated[7].target_identity = UINT64_C(0xcccc);
    atomic_store_explicit(&authenticated[7].sequence, UINT64_C(2), memory_order_relaxed);
    hl_native_ibtc_authenticated_storage_clear(authenticated);
    CHECK(authenticated[7].target == 0 && authenticated[7].authenticated_ingress == 0 &&
          authenticated[7].target_identity == 0 &&
          atomic_load_explicit(&authenticated[7].sequence, memory_order_relaxed) == 0);
    hl_native_ibtc_authenticated_storage_destroy(authenticated);

    CHECK(hl_native_create(&config, &executor) == HL_NATIVE_OK);
    CHECK(executor->authenticated_ibtc == NULL);
    hl_native_ibtc_fill_shared(executor, UINT64_C(0x1234), (void *)(uintptr_t)UINT64_C(0x5678));
    CHECK(executor->ibtc[(UINT64_C(0x1234) >> 2) & (HL_NATIVE_IBTC_COUNT - 1)].target == UINT64_C(0x1234));
    CHECK(hl_native_synchronize_epoch(executor, 0, 0, 1, 40) == HL_NATIVE_OK);
    CHECK(executor->authenticated_ibtc == NULL);
    CHECK(executor->memory_mode == 1 && executor->authority_generation == 40);
    CHECK(executor->ibtc[(UINT64_C(0x1234) >> 2) & (HL_NATIVE_IBTC_COUNT - 1)].target == 0);
    CHECK(hl_native_synchronize_epoch(executor, 0, 0, 1, 41) == HL_NATIVE_OK);
    CHECK(executor->authority_generation == 41);
    CHECK(hl_native_direct_register(executor, &expected, &first) == HL_NATIVE_OK);
    CHECK(first != NULL && hl_native_direct_validate(executor, first, &observed));
    CHECK(memcmp(&observed, &expected, sizeof(expected)) == 0);
    hl_native_projection_view view = {
        .guest_first = expected.guest_first, .guest_last = expected.guest_last,
        .host_first = expected.host_first, .mapping_incarnation = expected.mapping_incarnation,
        .permissions = expected.permissions, .write_policy = HL_NATIVE_WRITE_EXACT,
    };
    hl_native_projection projection = {&view, 1, expected.mapping_incarnation, 0};
    uint64_t first_generation = hl_native_direct_generation(executor, first);
    uint64_t first_identity = hl_native_direct_identity(executor, first);
    CHECK(first_generation != 0);
    CHECK(first_identity != 0);
    CHECK(hl_native_direct_request_valid(executor, first, first_generation, first_identity, &projection));
    CHECK(!hl_native_direct_request_valid(executor, first, first_generation + 1, first_identity, &projection));
    CHECK(!hl_native_direct_request_valid(executor, first, first_generation, first_identity + 1, &projection));
    CHECK(!hl_native_direct_request_valid(executor,
        (const hl_native_direct_token *)(uintptr_t)UINT64_C(0x1234), first_generation, first_identity, &projection));
    view.host_first++;
    CHECK(!hl_native_direct_request_valid(executor, first, first_generation, first_identity, &projection));
    view.host_first--;
#if defined(__aarch64__)
    const uint32_t instruction = UINT32_C(0xd4000001);
    hl_native_source_span span = {UINT64_C(0x1000), (const uint8_t *)&instruction,
                                  sizeof(instruction), 7, 9};
    hl_native_source source = {&span, 1, 7, 9};
    hl_native_aarch64_cpu state = {.program = UINT64_C(0x1000)};
    hl_native_cpu cpu = {.abi = HL_NATIVE_ABI, .size = sizeof(cpu),
                         .architecture = HL_NATIVE_AARCH64, .state.aarch64 = &state};
    hl_native_run_request request = {
        .abi = HL_NATIVE_ABI, .size = sizeof(request), .architecture = HL_NATIVE_AARCH64,
        .mapping_epoch = 7, .budget = 0, .source = &source, .projection = &projection,
        .memory_mode = 1, .authority_generation = first_generation, .direct_token = first,
        .authority_identity = first_identity,
    };
    hl_native_exit output = {.abi = HL_NATIVE_ABI, .size = sizeof(output)};
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_OK);
    CHECK(output.kind == HL_NATIVE_EXIT_YIELD);
    request.authority_generation++;
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_STATE);
    request.authority_generation--;
    request.direct_token = (const hl_native_direct_token *)(uintptr_t)UINT64_C(0x1234);
    CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_STATE);
    request.direct_token = first;
#endif
    hl_native_execution execution = {0};
    CHECK(hl_native_execution_enter(executor, &execution) == HL_NATIVE_OK);
    CHECK(hl_native_direct_unregister(executor, first) == HL_NATIVE_STATE);
    CHECK(hl_native_execution_leave(&execution) == HL_NATIVE_OK);
    CHECK(hl_native_direct_register(executor, &expected, &second) == HL_NATIVE_STATE);
    CHECK(second == NULL);
    CHECK(hl_native_destroy(executor) == HL_NATIVE_STATE);

    hl_native_execution before_fork_execution = {0};
    CHECK(hl_native_execution_enter(executor, &before_fork_execution) == HL_NATIVE_OK);
    uint64_t before_fork_generation = hl_native_execution_generation(&before_fork_execution);
    CHECK(before_fork_generation != 0);
    CHECK(hl_native_execution_leave(&before_fork_execution) == HL_NATIVE_OK);

    CHECK(hl_native_before_fork(executor) == HL_NATIVE_OK);
    pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
        CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
        hl_native_execution child_execution = {0};
        CHECK(hl_native_execution_enter(executor, &child_execution) == HL_NATIVE_OK);
        CHECK(hl_native_execution_generation(&child_execution) > before_fork_generation);
        CHECK(hl_native_execution_leave(&child_execution) == HL_NATIVE_OK);
        CHECK(executor->memory_mode == 0 && executor->authority_generation == 0);
        CHECK(!hl_native_direct_validate(executor, first, &observed));
#if defined(__aarch64__)
        state.program = UINT64_C(0x1000);
        CHECK(hl_native_run(executor, &cpu, &request, &output) == HL_NATIVE_STATE);
#endif
        CHECK(hl_native_direct_unregister(executor, first) == HL_NATIVE_OK);
        CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
        _exit(0);
    }
    int status;
    CHECK(waitpid(child, &status, 0) == child && WIFEXITED(status) && WEXITSTATUS(status) == 0);
    CHECK(hl_native_after_fork(executor, 1) == HL_NATIVE_OK);
    hl_native_execution parent_execution = {0};
    CHECK(hl_native_execution_enter(executor, &parent_execution) == HL_NATIVE_OK);
    CHECK(hl_native_execution_generation(&parent_execution) > before_fork_generation);
    CHECK(hl_native_execution_leave(&parent_execution) == HL_NATIVE_OK);
    CHECK(executor->memory_mode == 0 && executor->authority_generation == 0);
    CHECK(!hl_native_direct_validate(executor, first, &observed));
    CHECK(hl_native_direct_register(executor, &expected, &second) == HL_NATIVE_OK);
    CHECK(hl_native_direct_validate(executor, second, &observed));
    CHECK(hl_native_direct_unregister(executor, first) == HL_NATIVE_OK);
    CHECK(hl_native_direct_validate(executor, second, &observed));
    uint64_t second_generation = hl_native_direct_generation(executor, second);
    uint64_t second_identity = hl_native_direct_identity(executor, second);
    CHECK(second_generation != first_generation && second_identity != first_identity);
    CHECK(hl_native_synchronize_epoch(executor, 0, 0, 1, second_identity) == HL_NATIVE_OK);
    hl_native_ibtc_fill_shared(executor, UINT64_C(0x1234), (void *)(uintptr_t)UINT64_C(0x5678));
    CHECK(hl_native_direct_unregister(executor, second) == HL_NATIVE_OK);
    CHECK(executor->memory_mode == 1 && executor->authority_generation == second_identity);
    CHECK(executor->ibtc[(UINT64_C(0x1234) >> 2) & (HL_NATIVE_IBTC_COUNT - 1)].target == UINT64_C(0x1234));
    CHECK(hl_native_direct_register(executor, &expected, &third) == HL_NATIVE_OK);
    CHECK(hl_native_direct_generation(executor, third) != second_generation);
    CHECK(hl_native_direct_identity(executor, third) == second_identity);
    CHECK(!hl_native_direct_request_valid(executor, third, second_generation, second_identity, &projection));
    CHECK(hl_native_direct_unregister(executor, third) == HL_NATIVE_OK);
    hl_native_direct_authority variants[7];
    for (size_t index = 0; index < 7; ++index) variants[index] = expected;
    variants[0].guest_first++;
    variants[1].guest_last++;
    variants[2].host_first++;
    variants[3].permissions = HL_NATIVE_ACCESS_WRITE;
    variants[4].mapping_incarnation++;
    variants[5].mapping_generation++;
    variants[6].instruction_generation++;
    for (size_t index = 0; index < 7; ++index) {
        CHECK(hl_native_direct_register(executor, &variants[index], &third) == HL_NATIVE_OK);
        CHECK(hl_native_direct_identity(executor, third) != second_identity);
        CHECK(hl_native_direct_unregister(executor, third) == HL_NATIVE_OK);
    }
    CHECK(hl_native_destroy(executor) == HL_NATIVE_OK);
    return 0;
}
