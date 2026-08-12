#include "support.h"
#include "../src/arch/x86_64/frontend.h"

#include <stdio.h>
#include <string.h>

#define CHECK(value) do { if (!(value)) { fprintf(stderr, "x86_request_flags:%d: %s\n", __LINE__, #value); return 1; } } while (0)

static int flags_contract(void) {
    static const uint8_t code[] = {0x0f, 0x05};
    uint32_t words[64];
    hl_x86_a64_provenance provenance[8];
    hl_x86_a64_result result = {.abi = HL_X86_A64_FRONTEND_ABI, .size = sizeof(result)};
    hl_x86_a64_request request = {.abi = HL_X86_A64_FRONTEND_ABI,
                                  .size = sizeof(request),
                                  .guest_pc = 0x7000,
                                  .guest_bytes = code,
                                  .guest_size = sizeof(code),
                                  .max_instructions = 8,
                                  .host_words = words,
                                  .host_capacity = 64,
                                  .provenance = provenance,
                                  .provenance_capacity = 8,
                                  .flags = 0};

    /* Every named capability flag is accepted, so adding one without naming it in
     * HL_X86_A64_FLAGS is reported as an unknown bit rather than a decoder fault. */
    CHECK(hl_x86_a64_unknown_flags(HL_X86_A64_FLAGS) == 0u);
    CHECK(hl_x86_a64_unknown_flags(HL_X86_A64_AES) == 0u);

    uint32_t stray = (uint32_t)HL_X86_A64_FLAGS + 1u;
    stray &= ~(uint32_t)HL_X86_A64_FLAGS;
    CHECK(stray != 0u);
    CHECK(hl_x86_a64_unknown_flags(stray) == stray);

    request.flags = stray;
    hl_x86_a64_status status = hl_x86_a64_emit(&request, &result);
    if (status != HL_X86_A64_UNKNOWN_FLAG) {
        fprintf(stderr, "x86_request_flags: status %d for unknown flag bits %#x\n", (int)status, stray);
        return 1;
    }
    fprintf(stderr, "x86_request_flags: unknown flag bits %#x reported\n",
            hl_x86_a64_unknown_flags(request.flags));

    /* An ordinary rejection stays HL_X86_A64_ARGUMENT so the two causes remain distinguishable. */
    request.flags = 0;
    request.reserved = 1;
    CHECK(hl_x86_a64_emit(&request, &result) == HL_X86_A64_ARGUMENT);
    return 0;
}

int main(void) { return flags_contract(); }
