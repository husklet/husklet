#include "../src/arch/aarch64/frontend.h"

#include <stdint.h>
#include <stdio.h>

#define CHECK(expression)                                                                                              \
    do {                                                                                                               \
        if (!(expression)) {                                                                                           \
            fprintf(stderr, "line %d: %s\n", __LINE__, #expression);                                                 \
            return 1;                                                                                                  \
        }                                                                                                              \
    } while (0)

static int expect_control(uint64_t pc, uint32_t word, hl_a64_control control, unsigned source, uint64_t link) {
    hl_a64_instruction instruction = {pc, word, 0};
    uint8_t code[1] = {0xa5};
    hl_a64_provenance provenance;
    hl_a64_block_input input = {&instruction, 1, code, sizeof(code), UINT64_MAX, &provenance, 1};
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_CONTROL && output.control == control);
    CHECK(output.terminal_pc == pc && output.terminal_word == word);
    CHECK(output.control_register == source && output.control_link == link);
    CHECK(output.control_target == 0 && output.code_size == 0 && output.provenance_count == 0);
    CHECK(code[0] == 0xa5);
    return 0;
}

static int canonical_metadata(void) {
    CHECK(expect_control(0x4000, 0xd61f0000u, HL_A64_INDIRECT_BRANCH, 0, 0) == 0);
    CHECK(expect_control(0x4000, 0xd63f01e0u, HL_A64_INDIRECT_CALL, 15, 0x4004) == 0);
    CHECK(expect_control(0x4000, 0xd65f03e0u, HL_A64_RETURN, 31, 0) == 0);
    CHECK(expect_control(UINT64_C(0xfffffffffffffffc), 0xd63f03e0u, HL_A64_INDIRECT_CALL, 31, 0) == 0);
    return 0;
}

static int exact_fallback(uint32_t word) {
    hl_a64_instruction instruction = {0x8000, word, 0};
    uint8_t code[1] = {0xa5};
    hl_a64_provenance provenance;
    hl_a64_block_input input = {&instruction, 1, code, sizeof(code), UINT64_MAX, &provenance, 1};
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_UNSUPPORTED && output.control == HL_A64_CONTROL_NONE);
    CHECK(output.terminal_pc == 0x8000 && output.terminal_word == word);
    CHECK(output.code_size == 0 && code[0] == 0xa5);
    return 0;
}

static int stolen_sources(void) {
    static const unsigned reserved[] = {16, 17, 18, 28, 30};
    static const uint32_t bases[] = {0xd61f0000u, 0xd63f0000u, 0xd65f0000u};
    size_t reg;
    for (reg = 0; reg < sizeof(reserved) / sizeof(reserved[0]); reg++) {
        size_t family;
        for (family = 0; family < sizeof(bases) / sizeof(bases[0]); family++)
            CHECK(exact_fallback(bases[family] | (reserved[reg] << 5)) == 0);
    }
    return 0;
}

static int reserved_encodings(void) {
    static const uint32_t words[] = {
        0xd61f0001u, /* op4 */
        0xd61f0400u, /* op3 */
        0xd61e0000u, /* op2 */
        0xd67f0000u, /* ERET/DRPS opcode */
        0xd71f0800u, /* pointer-authenticated branch box */
    };
    size_t index;
    for (index = 0; index < sizeof(words) / sizeof(words[0]); index++) CHECK(exact_fallback(words[index]) == 0);
    return 0;
}

int main(void) {
    if (canonical_metadata() != 0) return 1;
    if (stolen_sources() != 0) return 1;
    if (reserved_encodings() != 0) return 1;
    return 0;
}
