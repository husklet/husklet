#include "../src/arch/aarch64/frontend.h"

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#define CHECK(expression)                                                                                              \
    do {                                                                                                               \
        if (!(expression)) {                                                                                           \
            fprintf(stderr, "line %d: %s\n", __LINE__, #expression);                                                 \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

static uint32_t read_word(const uint8_t *bytes) {
    return (uint32_t)bytes[0] | ((uint32_t)bytes[1] << 8) | ((uint32_t)bytes[2] << 16) |
           ((uint32_t)bytes[3] << 24);
}

static uint64_t read_constant(const uint8_t *bytes) {
    uint64_t value = (read_word(bytes) >> 5) & 0xffffu;
    unsigned half;
    for (half = 1; half < 4; half++)
        value |= (uint64_t)((read_word(bytes + half * 4) >> 5) & 0xffffu) << (half * 16);
    return value;
}

static int materializes_guest_address(void) {
    const hl_a64_instruction instructions[] = {
        {0x4120, 0x70000903u, 0}, /* adr x3, #0x123 */
        {0x4124, 0xd0000004u, 0}, /* adrp x4, #0x2000 */
    };
    uint8_t code[32] = {0};
    hl_a64_provenance provenance[2] = {{0}};
    hl_a64_block_input input = {instructions, 2, code, sizeof(code), 0x8000, provenance, 2};
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_COMPLETE && output.code_size == 32);
    CHECK(output.instruction_count == 2 && output.provenance_count == 2);
    CHECK(provenance[0].host_first == 0x8000 && provenance[0].host_last == 0x8010);
    CHECK(provenance[1].host_first == 0x8010 && provenance[1].host_last == 0x8020);
    CHECK(read_word(code) == 0xd2884863u); /* movz x3, #0x4243 */
    CHECK(read_word(code + 4) == 0xf2a00003u);
    CHECK(read_word(code + 16) == 0xd28c0004u); /* movz x4, #0x6000 */
    CHECK(read_word(code + 20) == 0xf2a00004u);
    return 0;
}

static int immediate_extremes(void) {
    const hl_a64_instruction instructions[] = {
        {0, 0x707fffe0u, 0}, /* adr x0, #0xfffff */
        {4, 0x10800001u, 0}, /* adr x1, #-0x100000 */
        {8, 0xf07fffe2u, 0}, /* adrp x2, #0xfffff000 */
        {12, 0x90800003u, 0}, /* adrp x3, #-0x100000000 */
    };
    uint8_t code[64] = {0};
    hl_a64_provenance provenance[4] = {{0}};
    hl_a64_block_input input = {instructions, 4, code, sizeof(code), 0xa000, provenance, 4};
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_COMPLETE && output.code_size == sizeof(code));
    CHECK(read_constant(code) == UINT64_C(0xfffff));
    CHECK(read_constant(code + 16) == UINT64_C(0xfffffffffff00004));
    CHECK(read_constant(code + 32) == UINT64_C(0xfffff000));
    CHECK(read_constant(code + 48) == UINT64_C(0xffffffff00000000));
    CHECK(provenance[3].host_first == 0xa030 && provenance[3].host_last == 0xa040);
    return 0;
}

static int adrp_page_wrap(void) {
    const hl_a64_instruction instruction = {UINT64_C(0xfffffffffffff000), 0xb0000006u, 0};
    uint8_t code[16] = {0};
    hl_a64_provenance provenance = {0};
    hl_a64_block_input input = {&instruction, 1, code, sizeof(code), 0xb000, &provenance, 1};
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(read_constant(code) == 0);
    return 0;
}

static int reserved_destinations(void) {
    static const unsigned reserved[] = {16, 17, 18, 28, 30};
    size_t index;
    for (index = 0; index < sizeof(reserved) / sizeof(reserved[0]); index++) {
        hl_a64_instruction instruction = {0x7000, UINT32_C(0x10000000) | reserved[index], 0};
        uint8_t code[16];
        hl_a64_provenance provenance;
        hl_a64_block_input input = {&instruction, 1, code, sizeof(code), 0xc000, &provenance, 1};
        hl_a64_block_output output;
        memset(code, 0xa5, sizeof(code));
        CHECK(hl_a64_emit_block(&input, &output));
        CHECK(output.terminal == HL_A64_UNSUPPORTED && output.terminal_pc == 0x7000);
        CHECK(output.code_size == 0 && code[0] == 0xa5);
    }
    return 0;
}

static int negative_displacement_wraps(void) {
    const hl_a64_instruction instruction = {0, 0x10ffffe5u, 0}; /* adr x5, #-4 */
    uint8_t code[16] = {0};
    hl_a64_provenance provenance = {0};
    hl_a64_block_input input = {&instruction, 1, code, sizeof(code), 0x9000, &provenance, 1};
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_COMPLETE && output.code_size == 16);
    CHECK(read_word(code) == 0xd29fff85u);
    CHECK(read_word(code + 4) == 0xf2bfffe5u);
    CHECK(read_word(code + 8) == 0xf2dfffe5u);
    CHECK(read_word(code + 12) == 0xf2ffffe5u);
    return 0;
}

static int expansion_is_failure_atomic(void) {
    hl_a64_instruction instruction = {0x7000, 0x10000010u, 0}; /* adr x16, #0 */
    uint8_t code[16];
    hl_a64_provenance provenance;
    hl_a64_block_input input = {&instruction, 1, code, sizeof(code), UINT64_MAX, &provenance, 1};
    hl_a64_block_output output;
    memset(code, 0xa5, sizeof(code));
    memset(&provenance, 0xa5, sizeof(provenance));
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_UNSUPPORTED && output.terminal_pc == 0x7000);
    CHECK(output.code_size == 0 && code[0] == 0xa5);
    instruction.word = 0x10000000u; /* adr x0, #0 */
    input.code_capacity = 15;
    input.host_base = 0xd000;
    CHECK(!hl_a64_emit_block(&input, &output));
    CHECK(code[0] == 0xa5 && provenance.host_first == UINT64_C(0xa5a5a5a5a5a5a5a5));
    input.code_capacity = sizeof(code);
    input.host_base = UINT64_MAX - 14;
    CHECK(!hl_a64_emit_block(&input, &output));
    CHECK(code[0] == 0xa5);
    return 0;
}

int main(void) {
    if (materializes_guest_address() != 0) return 1;
    if (immediate_extremes() != 0) return 1;
    if (adrp_page_wrap() != 0) return 1;
    if (reserved_destinations() != 0) return 1;
    if (negative_displacement_wraps() != 0) return 1;
    if (expansion_is_failure_atomic() != 0) return 1;
    return 0;
}
