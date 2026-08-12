enum { INTERP_SIMD_UNHANDLED = 2 };

#include "simd/crypto.c"
#include "simd/copy.c"
#include "simd/immediate.c"
#include "simd/shift.c"
#include "simd/rearrange.c"
#include "simd/fp16.c"
#include "simd/two/float.c"
#include "simd/two/unary.c"
#include "simd/two/integer.c"
#include "simd/three_different.c"
#include "simd/three/float.c"
#include "simd/three/shift.c"
#include "simd/three/pairwise.c"
#include "simd/three/integer.c"
#include "simd/extra.c"
#include "simd/indexed.c"

static int interp_exec_simd(struct cpu *cpu, uint32_t insn) {
    unsigned q = (insn >> 30) & 1, u = (insn >> 29) & 1;
    int status = interp_simd_crypto(cpu, insn, insn, 0, q, u);
    if (status != INTERP_SIMD_UNHANDLED) return status;

    // Scalar FP has a distinct decoder and must precede AdvSIMD scalar normalization.
    if ((insn & 0x7F000000u) == 0x1E000000u || (insn & 0x7F000000u) == 0x1F000000u)
        return interp_exec_fp_scalar(cpu, insn);

    unsigned scalar = 0;
    uint32_t decode = insn;
    if ((insn & 0xDE000000u) == 0x5E000000u) {
        scalar = 1;
        decode &= ~UINT32_C(0x50000000);
        q = 0;
    }

#define INTERP_SIMD_TRY(family)                                                                                        \
    do {                                                                                                               \
        status = interp_simd_##family(cpu, insn, decode, scalar, q, u);                                                \
        if (status != INTERP_SIMD_UNHANDLED) return status;                                                            \
    } while (0)

    INTERP_SIMD_TRY(copy);
    INTERP_SIMD_TRY(immediate);
    INTERP_SIMD_TRY(shift);
    INTERP_SIMD_TRY(rearrange);
    INTERP_SIMD_TRY(fp16);
    INTERP_SIMD_TRY(two_register);
    INTERP_SIMD_TRY(three_different);
    INTERP_SIMD_TRY(three_same);
    INTERP_SIMD_TRY(extra);
    INTERP_SIMD_TRY(indexed);

#undef INTERP_SIMD_TRY
    return interp_undefined(cpu, insn, "scalar floating-point and Advanced SIMD");
}
