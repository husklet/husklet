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

static int expect_predicate(uint64_t pc, uint32_t word, hl_a64_control control, uint64_t target,
                            unsigned reg, unsigned bit, unsigned wide, unsigned nonzero) {
    hl_a64_instruction instruction = {pc, word, 0};
    uint8_t code[1] = {0xa5};
    hl_a64_provenance provenance;
    hl_a64_block_input input = {&instruction, 1, code, sizeof(code), UINT64_MAX, &provenance, 1};
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_CONTROL && output.control == control);
    CHECK(output.terminal_pc == pc && output.terminal_word == word);
    CHECK(output.control_target == target && output.control_fallthrough == pc + 4);
    CHECK(output.control_register == reg && output.control_bit == bit);
    CHECK(output.control_wide == wide && output.control_nonzero == nonzero);
    CHECK(output.code_size == 0 && output.provenance_count == 0 && code[0] == 0xa5);
    return 0;
}

static int compare_metadata(void) {
    CHECK(expect_predicate(0x4000, 0x34000020u, HL_A64_COMPARE_BRANCH, 0x4004, 0, 0, 0, 0) == 0);
    CHECK(expect_predicate(0x4000, 0xb5000021u, HL_A64_COMPARE_BRANCH, 0x4004, 1, 0, 1, 1) == 0);
    CHECK(expect_predicate(0, 0x347fffe2u, HL_A64_COMPARE_BRANCH, 0x000ffffc, 2, 0, 0, 0) == 0);
    CHECK(expect_predicate(0, 0xb5800003u, HL_A64_COMPARE_BRANCH, UINT64_C(0xfffffffffff00000), 3, 0, 1, 1) == 0);
    CHECK(expect_predicate(UINT64_C(0xfffffffffffffffc), 0x3400003fu, HL_A64_COMPARE_BRANCH, 0,
                           31, 0, 0, 0) == 0);
    return 0;
}

static int test_metadata(void) {
    CHECK(expect_predicate(0x8000, 0x36000024u, HL_A64_TEST_BRANCH, 0x8004, 4, 0, 0, 0) == 0);
    CHECK(expect_predicate(0x8000, 0xb7f80025u, HL_A64_TEST_BRANCH, 0x8004, 5, 63, 1, 1) == 0);
    CHECK(expect_predicate(0, 0x363bffe6u, HL_A64_TEST_BRANCH, 0x00007ffc, 6, 7, 0, 0) == 0);
    CHECK(expect_predicate(0, 0xb7440007u, HL_A64_TEST_BRANCH, UINT64_C(0xffffffffffff8000),
                           7, 40, 1, 1) == 0);
    return 0;
}

static int stolen_fallback(void) {
    static const unsigned reserved[] = {16, 17, 18, 28, 30};
    size_t index;
    for (index = 0; index < sizeof(reserved) / sizeof(reserved[0]); index++) {
        uint32_t families[] = {UINT32_C(0xb5000020) | reserved[index], UINT32_C(0xb7000020) | reserved[index]};
        size_t family;
        for (family = 0; family < sizeof(families) / sizeof(families[0]); family++) {
            hl_a64_instruction instruction = {0x9000, families[family], 0};
            uint8_t code[1] = {0xa5};
            hl_a64_provenance provenance;
            hl_a64_block_input input = {&instruction, 1, code, sizeof(code), UINT64_MAX, &provenance, 1};
            hl_a64_block_output output;
            CHECK(hl_a64_emit_block(&input, &output));
            CHECK(output.terminal == HL_A64_UNSUPPORTED && output.terminal_pc == 0x9000);
            CHECK(output.control == HL_A64_CONTROL_NONE && output.terminal_word == families[family]);
            CHECK(output.code_size == 0 && code[0] == 0xa5);
        }
    }
    return 0;
}

static int expect_condition(uint64_t pc, uint32_t word, uint64_t target, unsigned condition) {
    hl_a64_instruction instruction = {pc, word, 0};
    uint8_t code[1] = {0xa5};
    hl_a64_provenance provenance;
    hl_a64_block_input input = {&instruction, 1, code, sizeof(code), UINT64_MAX, &provenance, 1};
    hl_a64_block_output output;
    CHECK(hl_a64_emit_block(&input, &output));
    CHECK(output.terminal == HL_A64_CONTROL && output.control == HL_A64_CONDITION_BRANCH);
    CHECK(output.terminal_pc == pc && output.terminal_word == word);
    CHECK(output.control_target == target && output.control_fallthrough == pc + 4);
    CHECK(output.control_condition == condition);
    CHECK(output.code_size == 0 && output.provenance_count == 0 && code[0] == 0xa5);
    return 0;
}

static int condition_metadata(void) {
    CHECK(expect_condition(0x6000, 0x54000020u, 0x6004, 0) == 0);
    CHECK(expect_condition(0, 0x547fffeeu, 0x000ffffc, 14) == 0);
    CHECK(expect_condition(0, 0x5480000fu, UINT64_C(0xfffffffffff00000), 15) == 0);
    CHECK(expect_condition(UINT64_C(0xfffffffffffffffc), 0x5400002fu, 0, 15) == 0);
    return 0;
}

int main(void) {
    if (condition_metadata() != 0) return 1;
    if (compare_metadata() != 0) return 1;
    if (test_metadata() != 0) return 1;
    if (stolen_fallback() != 0) return 1;
    return 0;
}
