#ifndef HL_NATIVE_X86_64_DECODE_H
#define HL_NATIVE_X86_64_DECODE_H

#include <limits.h>
#include <stddef.h>
#include <stdint.h>

#include "frontend.h"

enum operation {
    OP_NOP,
    OP_MOV_IMMEDIATE,
    OP_MOV_REGISTER,
    OP_EXTEND,
    OP_BSWAP,
    OP_BITSCAN,
    OP_BIT,
    OP_STRING,
    OP_BYTE,
    OP_ALU,
    OP_MUL,
    OP_DIV,
    OP_IMUL,
    OP_ROTATE,
    OP_SHIFT,
    OP_DOUBLE_SHIFT,
    OP_ADDRESS,
    OP_LOAD,
    OP_CMOV,
    OP_SET,
    OP_STORE,
    OP_XCHG,
    OP_XADD,
    OP_CMPXCHG,
    OP_CONTROL,
    OP_CALL,
    OP_JUMP,
    OP_RETURN,
    OP_LEAVE,
    OP_PUSH,
    OP_POP,
    OP_VECTOR,
    OP_ACCUMULATOR,
};

enum vector_operation {
    VECTOR_COPY,
    VECTOR_FROM_INTEGER,
    VECTOR_TO_INTEGER,
    VECTOR_ZERO_LOW,
    VECTOR_UNPACK_LOW,
    VECTOR_UNPACK_HIGH,
    VECTOR_INSERT_WORD,
    VECTOR_COMPARE_EQUAL,
    VECTOR_COMPARE_GREATER_SIGNED,
    VECTOR_MINIMUM_SIGNED,
    VECTOR_MINIMUM_UNSIGNED,
    VECTOR_MAXIMUM_SIGNED,
    VECTOR_MAXIMUM_UNSIGNED,
    VECTOR_BYTE_MASK,
    VECTOR_XOR,
    VECTOR_AND,
    VECTOR_OR,
    VECTOR_AND_NOT,
    VECTOR_ADD,
    VECTOR_SUBTRACT,
    VECTOR_MULTIPLY_LOW_WORD,
    VECTOR_MULTIPLY_HIGH_WORD,
    VECTOR_MULTIPLY_EVEN_DWORD,
    VECTOR_SCALAR_SQRT_DOUBLE,
    VECTOR_SCALAR_ADD_DOUBLE,
    VECTOR_SCALAR_MUL_DOUBLE,
    VECTOR_SCALAR_SUB_DOUBLE,
    VECTOR_SCALAR_DIV_DOUBLE,
    VECTOR_SCALAR_COMPARE_DOUBLE,
    VECTOR_TRUNC_DOUBLE_SIGNED,
    VECTOR_FLOAT_ARITHMETIC,
    VECTOR_SHUFFLE_DWORD,
    VECTOR_SHUFFLE_WORD,
    VECTOR_SHUFFLE_FLOAT,
    VECTOR_SHUFFLE_DOUBLE,
    VECTOR_SIGNED_DWORD_TO_FLOAT,
    VECTOR_FLOAT_TO_SIGNED_DWORD,
    VECTOR_TRUNC_FLOAT_TO_SIGNED_DWORD,
    VECTOR_STRING_EQUAL_EACH,
};

enum vector_immediate_form {
    VECTOR_IMMEDIATE_NONE,
    VECTOR_IMMEDIATE_RM_DESTRUCTIVE,
    VECTOR_IMMEDIATE_REG_DESTINATION,
};

typedef struct instruction {
    uint64_t pc;
    uint64_t immediate;
    uint64_t operand_immediate;
    uint8_t length;
    uint8_t operation;
    uint8_t destination;
    uint8_t source;
    uint8_t width;
    uint8_t condition;
    uint8_t source_high;
    uint8_t signed_extend;
    uint8_t destination_high;
    uint8_t has_immediate;
    uint8_t alu_kind;
    uint8_t flags_only;
    uint8_t preserve_carry;
    uint8_t preserve_flags;
    uint8_t unary_neg;
    uint8_t shift_kind;
    uint8_t shift_count;
    uint8_t variable_count;
    uint8_t shift_right;
    uint8_t conditional;
    uint8_t load_width;
    uint8_t memory_operand;
    uint8_t memory_write;
    uint8_t memory_destination;
    uint8_t address_32;
    uint8_t address_base;
    uint8_t address_index;
    uint8_t address_scale;
    uint8_t rip_relative;
    uint8_t segment;
    uint8_t vector_kind;
    uint8_t vector_lane;
    uint8_t vector_aligned;
    uint8_t vector_immediate_form;
    uint8_t vector_subopcode;
    uint8_t vector_immediate;
    uint8_t vector_memory_width;
    uint8_t vector_source_one;
    uint8_t vector_vex;
    uint8_t live_chain;
    uint8_t bit_index;
    uint8_t bit_memory_offset;
    uint8_t bit_operand_width;
    uint8_t bit_immediate;
} instruction;

typedef struct decode {
    instruction instructions[HL_X86_A64_MAX_INSTRUCTIONS];
    uint32_t count;
    uint64_t next_pc;
    uint64_t target;
    hl_x86_a64_exit exit;
    hl_x86_a64_status status;
} decode;

static inline uint64_t load_unsigned(const uint8_t *bytes, size_t count) {
    uint64_t value = 0;
    size_t index;

    for (index = 0; index < count; ++index) value |= (uint64_t)bytes[index] << (index * CHAR_BIT);
    return value;
}

static inline int64_t load_signed(const uint8_t *bytes, size_t count) {
    uint64_t value = load_unsigned(bytes, count);
    uint64_t sign = UINT64_C(1) << (count * CHAR_BIT - 1);

    return (int64_t)((value ^ sign) - sign);
}

static inline int add_pc(uint64_t pc, uint64_t amount, uint64_t *result) {
    if (amount > UINT64_MAX - pc) return 0;
    *result = pc + amount;
    return 1;
}

static inline int branch_target(uint64_t next, int64_t displacement, uint64_t *target) {
    uint64_t magnitude;

    if (displacement >= 0) return add_pc(next, (uint64_t)displacement, target);
    magnitude = (uint64_t)(-(displacement + 1)) + 1;
    if (magnitude > next) return 0;
    *target = next - magnitude;
    return 1;
}

#endif
