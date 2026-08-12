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

static int straight_line(void) {
    const hl_a64_instruction instructions[] = {
        {0x4000, 0xd2800020u, 0}, /* movz x0, #1 */
        {0x4004, 0x91000800u, 0}, /* add x0, x0, #2 */
        {0x4008, 0x8b010000u, 0}, /* add x0, x0, x1 */
    };
    uint8_t code[sizeof(instructions) / sizeof(instructions[0]) * 4];
    hl_a64_provenance provenance[3];
    hl_a64_block_input input = {
        .instructions = instructions,
        .instruction_count = 3,
        .code = code,
        .code_capacity = sizeof(code),
        .host_base = 0x8000,
        .provenance = provenance,
        .provenance_capacity = 3,
    };
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_COMPLETE);
    CHECK(output.code_size == 12 && output.instruction_count == 3 && output.provenance_count == 3);
    CHECK(provenance[0].host_first == 0x8000 && provenance[0].host_last == 0x8004);
    CHECK(provenance[2].host_first == 0x8008 && provenance[2].guest_pc == 0x4008);
    CHECK(code[0] == 0x20 && code[1] == 0x00 && code[2] == 0x80 && code[3] == 0xd2);
    return 0;
}

static int typed_stop(void) {
    const hl_a64_instruction instructions[] = {
        {0x5000, 0x91000400u, 0}, /* add x0, x0, #1 */
        {0x5004, 0xd4000001u, 0}, /* svc #0 */
        {0x5008, 0x91000800u, 0},
    };
    uint8_t code[12] = {0};
    hl_a64_provenance provenance[3] = {{0}};
    hl_a64_block_input input = {instructions, 3, code, sizeof(code), 0x9000, provenance, 3};
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_SYSCALL);
    CHECK(output.terminal_index == 1 && output.terminal_pc == 0x5004);
    CHECK(output.code_size == 4 && output.provenance_count == 1);
    CHECK(provenance[1].host_first == 0 && code[4] == 0);
    return 0;
}

static int guarded_boundaries(void) {
    hl_a64_instruction instruction = {0x6000, 0x9100079cu, 0}; /* add x28, x28, #1 */
    uint8_t code[4] = {0};
    hl_a64_provenance provenance = {0};
    hl_a64_block_input input = {&instruction, 1, code, sizeof(code), UINT64_MAX - 3, &provenance, 1};
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_UNSUPPORTED && output.code_size == 0);
    instruction.word = 0xf9400020u; /* ldr x0, [x1]: memory remains a later typed path */
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_UNSUPPORTED && output.terminal_pc == 0x6000);
    instruction.word = 0x91000420u; /* add x0, x1, #1 */
    input.code_capacity = 3;
    CHECK(!hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_UNSUPPORTED && output.code_size == 0);
    return 0;
}

int main(void) {
    if (straight_line() != 0) return 1;
    if (typed_stop() != 0) return 1;
    if (guarded_boundaries() != 0) return 1;
    return 0;
}
