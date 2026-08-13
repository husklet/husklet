#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_SSE_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_SSE_H

#include <stdint.h>

#include "../decoder.h"
#include "crypto.h"

int lower_sse_packed_binary(struct insn *, uint64_t, int, int, int);
int lower_sse_widening_multiply(struct insn *, uint64_t, int, int, int);
int lower_sse_shuffle(struct insn *, uint64_t, int, int, int);
int lower_sse_sign_mask(struct insn *, int, int);
int lower_mmx_fp_conversion(struct insn *, uint64_t, int, int);
int lower_sse_packed_shift(struct insn *, uint64_t, uint64_t, int, int, int, int *, hl_x86_crypto_state *);
int lower_sse_packed_conversion(struct insn, uint64_t, int, int);
int lower_sse_nontemporal_store(struct insn *, uint64_t, uint64_t, int, int);
int lower_sse_word_lane(struct insn *, uint64_t, uint64_t, int, int, int, int *);
int lower_sse_widening_integer(struct insn *, uint64_t, int, int, int);
int lower_sse_saturating_pack(struct insn *, uint64_t, int, int, int);
int lower_sse_unpack(struct insn *, uint64_t, int, int, int);
int lower_sse_float_unpack(struct insn *, uint64_t, int, int, int);
int lower_sse_two_source_shuffle(struct insn *, uint64_t, int, int, int);
int lower_sse_move_lane(struct insn *, uint64_t, int, int);
int lower_sse_bitwise(struct insn *, uint64_t, int, int, int);
int lower_sse_packed_double_integer(struct insn *, uint64_t, int, int);
int lower_sse_flag_compare(struct insn, uint64_t, uint64_t, int, int);
int lower_sse_compare(struct insn, uint64_t, uint64_t, int, int);
int lower_sse_scalar_to_integer(struct insn *, uint64_t, uint64_t, int);
int lower_sse_integer_to_scalar(struct insn *, uint64_t, uint64_t, int);
int lower_sse_minmax(struct insn *, uint64_t, uint64_t, int, int);
#endif
