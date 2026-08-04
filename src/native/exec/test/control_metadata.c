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

static int direct_targets(void) {
    static const struct {
        uint64_t pc;
        uint32_t word;
        uint64_t target;
    } cases[] = {
        {0x4000, 0x14000001u, 0x4004},
        {0x4000, 0x17ffffffu, 0x3ffc},
        {0, 0x15ffffffu, 0x07fffffc},
        {0, 0x16000000u, UINT64_C(0xfffffffff8000000)},
        {UINT64_C(0xfffffffffffffffc), 0x14000001u, 0},
    };
    size_t index;
    for (index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        hl_a64_instruction instruction = {cases[index].pc, cases[index].word, 0};
        uint8_t code[1] = {0xa5};
        hl_a64_provenance provenance;
        hl_a64_block_input input = {&instruction, 1, code, sizeof(code), UINT64_MAX, &provenance, 1};
        hl_a64_block_output output;
        memset(&provenance, 0xa5, sizeof(provenance));
        CHECK(hl_a64_emit_block(&input, &output));
        CHECK(output.terminal == HL_A64_CONTROL && output.control == HL_A64_DIRECT_BRANCH);
        CHECK(output.control_link == 0);
        CHECK(output.terminal_index == 0 && output.terminal_pc == cases[index].pc);
        CHECK(output.terminal_word == cases[index].word && output.control_target == cases[index].target);
        CHECK(output.code_size == 0 && output.provenance_count == 0 && code[0] == 0xa5);
    }
    return 0;
}

static int dependent_control_falls_back(void) {
    static const uint32_t words[] = {
        0x54000010u, /* reserved B.cond bit4 */
        0xd61f0001u, /* reserved branch-register op4 */
    };
    size_t index;
    for (index = 0; index < sizeof(words) / sizeof(words[0]); index++) {
        hl_a64_instruction instruction = {0x8000, words[index], 0};
        uint8_t code[1] = {0xa5};
        hl_a64_provenance provenance;
        hl_a64_block_input input = {&instruction, 1, code, sizeof(code), UINT64_MAX, &provenance, 1};
        hl_a64_block_output output;
        CHECK(hl_a64_emit_block(&input, &output));
        CHECK(output.terminal == HL_A64_UNSUPPORTED && output.control == HL_A64_CONTROL_NONE);
        CHECK(output.terminal_pc == 0x8000 && output.terminal_word == words[index]);
        CHECK(output.code_size == 0 && code[0] == 0xa5);
    }
    return 0;
}

static int direct_calls(void) {
    static const struct {
        uint64_t pc;
        uint32_t word;
        uint64_t target;
        uint64_t link;
    } cases[] = {
        {0x5000, 0x94000001u, 0x5004, 0x5004},
        {0x5000, 0x97ffffffu, 0x4ffc, 0x5004},
        {0, 0x95ffffffu, 0x07fffffc, 4},
        {0, 0x96000000u, UINT64_C(0xfffffffff8000000), 4},
        {UINT64_C(0xfffffffffffffffc), 0x94000001u, 0, 0},
    };
    size_t index;
    for (index = 0; index < sizeof(cases) / sizeof(cases[0]); index++) {
        hl_a64_instruction instruction = {cases[index].pc, cases[index].word, 0};
        uint8_t code[1] = {0xa5};
        hl_a64_provenance provenance;
        hl_a64_block_input input = {&instruction, 1, code, sizeof(code), UINT64_MAX, &provenance, 1};
        hl_a64_block_output output;
        CHECK(hl_a64_emit_block(&input, &output));
        CHECK(output.terminal == HL_A64_CONTROL && output.control == HL_A64_DIRECT_CALL);
        CHECK(output.terminal_pc == cases[index].pc && output.terminal_word == cases[index].word);
        CHECK(output.control_target == cases[index].target && output.control_link == cases[index].link);
        CHECK(output.code_size == 0 && output.provenance_count == 0 && code[0] == 0xa5);
    }
    return 0;
}

int main(void) {
    if (direct_targets() != 0) return 1;
    if (direct_calls() != 0) return 1;
    if (dependent_control_falls_back() != 0) return 1;
    return 0;
}
