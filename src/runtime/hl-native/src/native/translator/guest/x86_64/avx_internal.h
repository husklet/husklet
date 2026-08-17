#ifndef HL_TRANSLATOR_GUEST_X86_64_AVX_INTERNAL_H
#define HL_TRANSLATOR_GUEST_X86_64_AVX_INTERNAL_H

#include "avx.h"
#include "cpu.h"
#include "decoder.h"

#include <stddef.h>
#include <stdint.h>

#define SSE_XI 0x01u
#define SSE_XD 0x02u
#define SSE_XZ 0x04u
#define SSE_XO 0x08u
#define SSE_XU 0x10u
#define SSE_XP 0x20u

enum avx_dispatch_result {
    AVX_DISPATCH_UNIMPLEMENTED = -1,
    AVX_DISPATCH_UNMATCHED = 0,
    AVX_DISPATCH_HANDLED = 1,
};

uint64_t avx_ea(const hl_x86_avx_state *state, struct cpu *cpu, struct insn *instruction, uint64_t next, int width);
int avx_memory_read(const hl_x86_avx_state *state, uint64_t guest, void *destination, size_t length);
int avx_memory_write(const hl_x86_avx_state *state, uint64_t guest, const void *source, size_t length);
void avx_get_rm(const hl_x86_avx_state *state, struct cpu *cpu, struct insn *instruction, uint64_t next, int width,
                uint8_t output[64]);
void avx_get(struct cpu *cpu, int register_index, uint8_t output[64]);
void avx_put(struct cpu *cpu, int register_index, const uint8_t input[64], int width);
void avx_undefined(void);
int avx_try_read(const hl_x86_avx_state *state, uint64_t guest, void *destination, size_t length);
void avx_abandon(uint64_t guest, uint64_t length, uint32_t required);
float avx_dnan_f32(float result, float left, float right);
double avx_dnan_f64(double result, double left, double right);
unsigned cvt_fp_flags(void);
void cvt_fp_flags_set(unsigned keep);
void hl_x86_sse_raise(unsigned mxcsr_bits);
int sse_daz_active(void);
int sse_is_denorm_f32(uint32_t bits);
int sse_is_denorm_f64(uint64_t bits);
float avx_fp_arith_f32(int op, float left, float right);
double avx_fp_arith_f64(int op, double left, double right);
float fma_x86_f32(float left, float right, float addend, int negate_multiply, int negate_addend);
double fma_x86_f64(double left, double right, double addend, int negate_multiply, int negate_addend);
int avx_cmp_pred(double left, double right, int predicate);
uint16_t avx_f32_to_f16(float value, int immediate);
float avx_f16_to_f32(uint16_t bits);
int64_t cvt_x86_d2i(double value, int truncate, int wide);
int64_t cvt_x86_f2i(float value, int truncate, int wide);
uint64_t simd_element_negate(uint64_t value, int size);
int simd_element_negative(uint64_t value, int size);
int sse_host_rounding_control(void);
double sse_round_d(double value, int immediate);
float sse_round_f(float value, int immediate);
void aes_subbytes(uint8_t state[16], const uint8_t box[256]);
void aes_shiftrows(const uint8_t input[16], uint8_t output[16], int inverse);
void aes_mixcolumns(uint8_t state[16], int inverse);
int sat_s16(int value);
extern const uint8_t k_aes_sbox[256];
extern const uint8_t k_aes_isbox[256];
void hl_x86_sse_execute(const hl_x86_avx_state *state, struct cpu *cpu);
enum avx_dispatch_result avx_dispatch_vector(const hl_x86_avx_state *state, struct cpu *cpu, struct insn *instruction,
                                             uint64_t next, int map, int op, int width);
enum avx_dispatch_result avx_dispatch_special(const hl_x86_avx_state *state, struct cpu *cpu, struct insn *instruction,
                                              uint64_t next, int map, int op, int prefix, int destination,
                                              int first_register, int width);

#endif
