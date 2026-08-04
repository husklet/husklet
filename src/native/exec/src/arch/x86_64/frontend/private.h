#ifndef HL_NATIVE_X86_64_FRONTEND_PRIVATE_H
#define HL_NATIVE_X86_64_FRONTEND_PRIVATE_H

#include <stdint.h>

#include "../frontend.h"

typedef struct instruction instruction;
typedef struct decode decode;

uint32_t hl_x86_alu_words(const instruction *item);
uint32_t hl_x86_bitscan_words(const instruction *item);
void hl_x86_emit_bitscan(uint32_t *words, uint32_t *cursor, const instruction *item);
int hl_x86_decode_bit(const hl_x86_a64_request *request, decode *block, instruction *item,
                      uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                      size_t start, size_t *cursor);
uint32_t hl_x86_bit_words(const instruction *item);
void hl_x86_emit_bit(uint32_t *words, uint32_t *cursor, const instruction *item);
uint32_t hl_x86_string_words(const instruction *item);
void hl_x86_emit_string(uint32_t *words, uint32_t *cursor, const instruction *item);
uint32_t hl_x86_bit(unsigned destination, unsigned source, unsigned lsb);
void hl_x86_emit_alu(uint32_t *words, uint32_t *cursor, const instruction *item);
uint32_t hl_x86_accumulator_words(const instruction *item);
void hl_x86_emit_accumulator(uint32_t *words, uint32_t *cursor, const instruction *item);
int hl_x86_decode_imul(const hl_x86_a64_request *request, decode *block, instruction *item,
                       uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                       size_t start, size_t *cursor);
uint32_t hl_x86_imul_words(const instruction *item);
void hl_x86_emit_imul(uint32_t *words, uint32_t *cursor, const instruction *item);
int hl_x86_decode_mul(const hl_x86_a64_request *request, decode *block, instruction *item,
                      uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                      size_t start, size_t *cursor);
uint32_t hl_x86_mul_words(const instruction *item);
void hl_x86_emit_mul(uint32_t *words, uint32_t *cursor, const instruction *item);
int hl_x86_decode_div(const hl_x86_a64_request *request, decode *block, instruction *item,
                      uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                      size_t start, size_t *cursor);
uint32_t hl_x86_div_words(const instruction *item);
void hl_x86_emit_div(uint32_t *words, uint32_t *cursor, const instruction *item);
int hl_x86_decode_nop(const hl_x86_a64_request *request, decode *block, instruction *item,
                      size_t start, size_t *cursor);
void hl_x86_emit_nzcv(uint32_t *words, uint32_t *cursor);
void hl_x86_emit_nzcv_keep_carry(uint32_t *words, uint32_t *cursor);
void hl_x86_emit_rflags(uint32_t *words, uint32_t *cursor);
int hl_x86_request_valid(const hl_x86_a64_request *request, const hl_x86_a64_result *result);
hl_x86_a64_status hl_x86_result(const hl_x86_a64_request *request, const decode *block,
                                hl_x86_a64_result *result, uint32_t words);
void hl_x86_spill(uint32_t *words, uint32_t *cursor, uint32_t dirty);
void hl_x86_spill_all(uint32_t *words, uint32_t *cursor);
void hl_x86_checkpoint(uint32_t *words, uint32_t *cursor, uint32_t delta, int live_chain);
void hl_x86_finish_chain(uint32_t *words, uint32_t *cursor);
void hl_x86_emit_exit(const hl_x86_a64_request *request, const decode *block, uint32_t *cursor);
int hl_x86_immediate_opcode(uint8_t opcode);
int hl_x86_decode_immediate(const hl_x86_a64_request *request, decode *block, instruction *item,
                            uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                            size_t start, size_t *cursor);
int hl_x86_test_opcode(uint8_t opcode);
int hl_x86_decode_test(const hl_x86_a64_request *request, decode *block, instruction *item,
                       uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                       uint8_t lock, size_t start, size_t *cursor);
int hl_x86_shift_opcode(uint8_t opcode);
int hl_x86_decode_shift(const hl_x86_a64_request *request, decode *block, instruction *item,
                        uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                        size_t start, size_t *cursor);
uint32_t hl_x86_shift_words(const instruction *item);
void hl_x86_emit_shift(uint32_t *words, uint32_t *cursor, const instruction *item);
int hl_x86_decode_double_shift(const hl_x86_a64_request *request, decode *block,
                               instruction *item, uint8_t opcode, uint8_t rex,
                               uint8_t operand_16, uint8_t address_32,
                               size_t start, size_t *cursor);
uint32_t hl_x86_double_shift_words(const instruction *item);
void hl_x86_emit_double_shift(uint32_t *words, uint32_t *cursor, const instruction *item);
uint32_t hl_x86_rotate_words(const instruction *item);
void hl_x86_emit_rotate(uint32_t *words, uint32_t *cursor, const instruction *item);
int hl_x86_decode_address(const hl_x86_a64_request *request, decode *block, instruction *item,
                          uint8_t rex, uint8_t operand_16, uint8_t address_32, size_t start, size_t *cursor);
int hl_x86_decode_store(const hl_x86_a64_request *request, decode *block, instruction *item,
                        uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                        size_t start, size_t *cursor);
int hl_x86_decode_load(const hl_x86_a64_request *request, decode *block, instruction *item,
                       uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                       size_t start, size_t *cursor);
uint32_t hl_x86_address_words(const instruction *item);
void hl_x86_emit_address(uint32_t *words, uint32_t *cursor, const instruction *item);
uint32_t hl_x86_load_words(const instruction *item);
void hl_x86_emit_load(uint32_t *words, uint32_t *cursor, const instruction *item);
uint32_t hl_x86_read_cache_words(void);
void hl_x86_emit_read_cache(uint32_t *words, uint32_t *cursor, unsigned width,
                            unsigned destination, int vector, uint32_t **hits);
void hl_x86_patch_read_hits(uint32_t **hits, uint32_t *target);
uint32_t hl_x86_cmov_words(const instruction *item);
void hl_x86_emit_cmov(uint32_t *words, uint32_t *cursor, const instruction *item, unsigned source);
uint32_t hl_x86_set_words(const instruction *item);
void hl_x86_emit_set(uint32_t *words, uint32_t *cursor, const instruction *item);
uint32_t hl_x86_store_words(const instruction *item);
void hl_x86_emit_dirty(uint32_t *words, uint32_t *cursor);
void hl_x86_emit_store(uint32_t *words, uint32_t *cursor, const instruction *item);
uint32_t hl_x86_xchg_words(const instruction *item);
void hl_x86_emit_xchg(uint32_t *words, uint32_t *cursor, const instruction *item);
int hl_x86_decode_xadd(const hl_x86_a64_request *request, decode *block, instruction *item,
                       uint8_t opcode, uint8_t rex, uint8_t operand_16, uint8_t address_32,
                       uint8_t lock, size_t start, size_t *cursor);
uint32_t hl_x86_xadd_words(const instruction *item);
void hl_x86_emit_xadd(uint32_t *words, uint32_t *cursor, const instruction *item);
uint32_t hl_x86_cmpxchg_words(const instruction *item);
void hl_x86_emit_cmpxchg(uint32_t *words, uint32_t *cursor, const instruction *item);
uint32_t hl_x86_vector_words(const instruction *item);
void hl_x86_emit_vector(uint32_t *words, uint32_t *cursor, const instruction *item);
void hl_x86_prepare_vector_immediate(instruction *item, uint8_t modrm, uint8_t rex,
                                     uint8_t immediate, uint8_t destructive_rm);
uint32_t hl_x86_control_words(const instruction *item);
void hl_x86_emit_control(uint32_t *words, uint32_t *cursor, const instruction *item);

#endif
