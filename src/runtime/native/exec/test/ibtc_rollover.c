#include "support.h"

#include "../src/executor.h"
#include "../cache/cache.h"

#include <stdio.h>
#include <string.h>

#define CHECK(expression)                                                                                              \
    do {                                                                                                               \
        if (!(expression)) {                                                                                           \
            fprintf(stderr, "ibtc_rollover:%d: %s\n", __LINE__, #expression);                                          \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

#if defined(__aarch64__)
static int stale_site_after_rollover(void) {
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    hl_native_config request = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_aarch64_cpu cpu = {0};
    hl_native_code code = {0};
    CHECK(hl_native_create(&request, &executor) == HL_NATIVE_OK);
    CHECK(hl_native_executor_gate_enter(executor) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_begin(executor->cache) == HL_NATIVE_OK);
    CHECK(hl_native_executor_rollover(executor, 1, 1) == HL_NATIVE_OK);
    CHECK(hl_native_cache_write_end(executor->cache) == HL_NATIVE_OK);
    hl_native_executor_gate_leave(executor);
    CHECK(executor->arena.mapping.content == 0);
    /* The site the rotated-away reservation handed out is stale, not corrupt. */
    cpu.indirect_site = (uint64_t)(uintptr_t)executor->arena.executable;
    CHECK(hl_native_ibtc_fill(executor, &cpu, 0x4000, &code) == HL_NATIVE_OK);
    CHECK(cpu.indirect_site == 0);
    hl_native_destroy(executor);
    return 0;
}

static int misaligned_site_stays_fatal(void) {
    test_memory host = {0};
    hl_native_memory memory = test_services(&host);
    hl_native_config request = test_config(&memory, 0);
    hl_native_executor *executor = NULL;
    hl_native_aarch64_cpu cpu = {0};
    hl_native_code code = {0};
    CHECK(hl_native_create(&request, &executor) == HL_NATIVE_OK);
    cpu.indirect_site = (uint64_t)(uintptr_t)executor->arena.executable + 4;
    CHECK(hl_native_ibtc_fill(executor, &cpu, 0x4000, &code) == HL_NATIVE_STATE);
    CHECK(strcmp(hl_native_state_invariant(), "ibtc site alignment") == 0);
    hl_native_destroy(executor);
    return 0;
}
#endif

int main(void) {
#if defined(__aarch64__)
    if (stale_site_after_rollover() != 0) return 1;
    if (misaligned_site_stays_fatal() != 0) return 1;
#endif
    return 0;
}
