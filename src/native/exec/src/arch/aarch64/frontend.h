#ifndef HL_NATIVE_AARCH64_FRONTEND_H
#define HL_NATIVE_AARCH64_FRONTEND_H

#include <stddef.h>
#include <stdint.h>

#define HL_A64_BLOCK_MAX_INSTRUCTIONS 64u

typedef struct hl_a64_instruction {
    uint64_t guest_pc;
    uint32_t word;
    uint32_t reserved;
} hl_a64_instruction;

typedef struct hl_a64_provenance {
    uint64_t host_first;
    uint64_t host_last;
    uint64_t guest_pc;
} hl_a64_provenance;

typedef enum hl_a64_terminal {
    HL_A64_COMPLETE = 0,
    HL_A64_SYSCALL = 1,
    HL_A64_CONTROL = 2,
    HL_A64_UNSUPPORTED = 3,
} hl_a64_terminal;

typedef enum hl_a64_control {
    HL_A64_CONTROL_NONE = 0,
    HL_A64_DIRECT_BRANCH = 1,
    HL_A64_DIRECT_CALL = 2,
    HL_A64_COMPARE_BRANCH = 3,
    HL_A64_TEST_BRANCH = 4,
    HL_A64_CONDITION_BRANCH = 5,
    HL_A64_INDIRECT_BRANCH = 6,
    HL_A64_INDIRECT_CALL = 7,
    HL_A64_RETURN = 8,
} hl_a64_control;

typedef struct hl_a64_block_input {
    const hl_a64_instruction *instructions;
    size_t instruction_count;
    uint8_t *code;
    size_t code_capacity;
    uint64_t host_base;
    hl_a64_provenance *provenance;
    size_t provenance_capacity;
} hl_a64_block_input;

typedef struct hl_a64_block_output {
    size_t code_size;
    size_t instruction_count;
    size_t provenance_count;
    size_t terminal_index;
    uint64_t terminal_pc;
    uint64_t control_target;
    uint64_t control_link;
    uint64_t control_fallthrough;
    uint32_t terminal_word;
    uint8_t control_register;
    uint8_t control_bit;
    uint8_t control_wide;
    uint8_t control_nonzero;
    uint8_t control_condition;
    hl_a64_terminal terminal;
    hl_a64_control control;
} hl_a64_block_output;

/* Emits a bounded same-ISA block fragment without fetching guest memory or
 * crossing an FFI boundary per instruction. The caller owns every input and
 * output buffer and must append the typed spill/exit selected by terminal.
 * Control target/link values describe required architectural publication;
 * this stateless call never mutates a CPU record or executes the edge. */
int hl_a64_emit_block(const hl_a64_block_input *, hl_a64_block_output *);

#endif
