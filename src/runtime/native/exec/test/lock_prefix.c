#include "../src/arch/x86_64/frontend.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define CHECK(expression) do { if (!(expression)) { \
    fprintf(stderr, "x86_lock_prefix:%d: %s\n", __LINE__, #expression); return __LINE__; \
} } while (0)

static hl_x86_a64_status emit(const uint8_t *guest, size_t size, uint64_t flags,
                              hl_x86_a64_result *result) {
    uint32_t host[512];
    hl_x86_a64_provenance provenance[2] = {0};
    hl_x86_a64_request request;
    memset(&request, 0, sizeof request);
    request.abi = HL_X86_A64_FRONTEND_ABI;
    request.size = sizeof request;
    request.guest_pc = UINT64_C(0x400000);
    request.guest_bytes = guest;
    request.guest_size = size;
    request.max_instructions = 1u;
    request.host_words = host;
    request.host_capacity = 512u;
    request.provenance = provenance;
    request.provenance_capacity = 2u;
    request.flags = flags;
    return hl_x86_a64_emit(&request, result);
}

int main(void) {
    static const uint8_t locked_memory[] = {0xf0u, 0x48u, 0x0fu, 0xb1u, 0x13u};
    static const uint8_t locked_register[] = {0xf0u, 0x48u, 0x0fu, 0xb1u, 0xd3u};
    static const uint8_t locked_unrelated[] = {0xf0u, 0x0fu, 0xc3u, 0x18u};
    static const uint8_t locked_xadd_memory[] = {0xf0u, 0x48u, 0x0fu, 0xc1u, 0x13u};
    static const uint8_t locked_xadd_register[] = {0xf0u, 0x48u, 0x0fu, 0xc1u, 0xd3u};
    hl_x86_a64_result result;

    CHECK(emit(locked_memory, sizeof locked_memory, HL_X86_A64_LSE, &result) == HL_X86_A64_OK);
    CHECK(result.instruction_count == 1u);
    CHECK(result.exit_pc == UINT64_C(0x400000) + sizeof locked_memory);
    CHECK(emit(locked_memory, sizeof locked_memory, 0u, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(emit(locked_register, sizeof locked_register, HL_X86_A64_LSE, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(emit(locked_xadd_memory, sizeof locked_xadd_memory, HL_X86_A64_LSE, &result) == HL_X86_A64_OK);
    CHECK(emit(locked_xadd_register, sizeof locked_xadd_register, HL_X86_A64_LSE, &result) == HL_X86_A64_UNSUPPORTED);
    CHECK(emit(locked_unrelated, sizeof locked_unrelated, HL_X86_A64_LSE, &result) == HL_X86_A64_UNSUPPORTED);
    return 0;
}
