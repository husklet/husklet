#ifndef HL_TRANSLATOR_GUEST_X86_64_LOWER_MOV_H
#define HL_TRANSLATOR_GUEST_X86_64_LOWER_MOV_H

#include <stdint.h>

struct insn;

typedef struct hl_x86_move_image {
    uint64_t low;
    uint64_t high;
    uint64_t bias;
    uint64_t types_low;
    uint64_t types_high;
    uint64_t blob_code;
} hl_x86_move_image;

int hl_x86_lower_mov(struct insn *insn, uint64_t next, const hl_x86_move_image *image);

#endif
