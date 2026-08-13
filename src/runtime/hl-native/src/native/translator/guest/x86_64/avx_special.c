#include "avx_internal.h"

#include <string.h>

static enum avx_dispatch_result avx_dispatch_bmi(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                 uint64_t next, int map, int op, int pp, int rd, int vv) {
    if (!((map == 2 && (op == 0xf2 || op == 0xf3 || op == 0xf5 || op == 0xf6 || op == 0xf7)) ||
          (map == 3 && op == 0xf0)))
        return AVX_DISPATCH_UNMATCHED;

    int wb = I->vex_w ? 64 : 32;
    uint64_t M = I->vex_w ? ~0ull : 0xffffffffull;
    uint64_t rm;
    if (I->is_mem) {
        uint64_t ea = avx_ea(state, c, I, next, I->vex_w ? 8 : 4);
        rm = 0;
        (void)avx_memory_read(state, ea, &rm, I->vex_w ? 8u : 4u);
    } else
        rm = c->r[I->rm_reg] & M;
    uint64_t v2 = c->r[vv] & M, res = 0;
    int setfl = 0, cf = 0, zf, sf, dest = rd;
    if (map == 2 && op == 0xf5 && pp == 0) { // BZHI rd, rm, vvvv: zero bits >= index(vvvv&0xff)
        int idx = (int)(v2 & 0xff);
        res = (idx >= wb) ? rm : (rm & ((idx == 0) ? 0 : ((1ull << idx) - 1)));
        cf = (idx > wb - 1);
        setfl = 1;
    } else if (map == 2 && op == 0xf7 && pp == 0) { // BEXTR rd, rm, vvvv(start:len in al:ah of vvvv)
        int start = (int)(v2 & 0xff), len = (int)((v2 >> 8) & 0xff);
        uint64_t t = (start >= wb) ? 0 : (rm >> start);
        res = (len >= wb) ? t : (t & ((len == 0) ? 0 : ((1ull << len) - 1)));
        setfl = 1;
    } else if (map == 2 && op == 0xf7 && pp == 1) { // SHLX rd, rm, vvvv
        res = rm << (v2 & (uint64_t)(wb - 1));
    } else if (map == 2 && op == 0xf7 && pp == 2) { // SARX rd, rm, vvvv (arithmetic)
        int sh = (int)(v2 & (uint64_t)(wb - 1));
        res = (uint64_t)(I->vex_w ? ((int64_t)rm >> sh) : ((int32_t)rm >> sh));
    } else if (map == 2 && op == 0xf7 && pp == 3) { // SHRX rd, rm, vvvv
        res = rm >> (v2 & (uint64_t)(wb - 1));
    } else if (map == 2 && op == 0xf6 && pp == 3) { // MULX rd(hi):vvvv(lo) = rdx * rm
// __int128 is a pre-C23 GNU/clang extension the widening multiply/carryless paths need; scope the
// -Wpedantic silence to the declaration rather than dropping the type.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
        unsigned __int128 p = (unsigned __int128)(c->r[RDX] & M) * (unsigned __int128)rm;
#pragma GCC diagnostic pop
        c->r[vv] = (uint64_t)p & M;
        res = (uint64_t)(I->vex_w ? (p >> 64) : ((p >> 32) & 0xffffffff));
    } else if (map == 3 && op == 0xf0 && pp == 3) { // RORX rd, rm, imm8 (no flags)
        int sh = (int)(I->imm & (wb - 1));
        res = sh ? ((rm >> sh) | (rm << (wb - sh))) : rm;
        if (!I->vex_w) res &= M;
    } else if (map == 2 && op == 0xf5 && pp == 2) { // PEXT rd, vvvv(src), rm(mask) -- F3 prefix => pp=2
        uint64_t src = v2, msk = rm, bit = 1;
        for (uint64_t m = msk; m; m &= m - 1) {
            if (src & (m & (~m + 1))) res |= bit;
            bit <<= 1;
        }
    } else if (map == 2 && op == 0xf5 && pp == 3) { // PDEP rd, vvvv(src), rm(mask) -- F2 prefix => pp=3
        uint64_t src = v2, msk = rm, bit = 1;
        for (uint64_t m = msk; m; m &= m - 1) {
            if (src & bit) res |= (m & (~m + 1));
            bit <<= 1;
        }
    } else if (map == 2 && op == 0xf2 && pp == 0) { // ANDN rd, vvvv, rm: (~src1) & src2; SF/ZF, CF=OF=0
        res = (~v2) & rm;
        cf = 0;
        setfl = 1;
    } else if (map == 2 && op == 0xf3 && pp == 0) { // BMI1 BLS group (ModRM.reg = opcode ext; dest in vvvv)
        int grp = I->reg & 7;
        dest = vv;
        if (grp == 1) { // BLSR vvvv, rm: (rm-1) & rm; CF=(rm==0)
            res = (rm - 1) & rm;
            cf = (rm == 0);
        } else if (grp == 2) { // BLSMSK vvvv, rm: (rm-1) ^ rm; CF=(rm==0)
            res = (rm - 1) ^ rm;
            cf = (rm == 0);
        } else if (grp == 3) { // BLSI vvvv, rm: (-rm) & rm; CF=(rm!=0)
            res = (0 - rm) & rm;
            cf = (rm != 0);
        } else {
            return AVX_DISPATCH_UNIMPLEMENTED;
        }
        setfl = 1;
    } else {
        return AVX_DISPATCH_UNIMPLEMENTED;
    }
    c->r[dest] = res & M; // 32-bit dest zero-extends to 64
    if (setfl) {          // BZHI/BEXTR/ANDN/BLS* set ZF/SF, CF as computed, OF=0
        zf = ((res & M) == 0);
        sf = (int)((res >> (wb - 1)) & 1);
        c->nzcv = ((uint64_t)sf << 31) | ((uint64_t)zf << 30) | ((uint64_t)(!cf) << 29);
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

static enum avx_dispatch_result avx_dispatch_fma(const hl_x86_avx_state *state, struct cpu *c, struct insn *I,
                                                 uint64_t next, int map, int op, int rd, int vv, int width) {
    int alternating = op == 0x96 || op == 0x97 || op == 0xA6 || op == 0xA7 || op == 0xB6 || op == 0xB7;
    int arithmetic = (op >= 0x98 && op <= 0x9F) || (op >= 0xA8 && op <= 0xAF) || (op >= 0xB8 && op <= 0xBF);
    if (map != 2 || (!alternating && !arithmetic)) return AVX_DISPATCH_UNMATCHED;

    int form = (op >> 4) - 9; // 0=132, 1=213, 2=231
    int dbl = I->vex_w;
    int element_size = dbl ? 8 : 4;
    uint8_t destination[64], vvvv[64], operand[64], output[64];
    avx_get(c, rd, destination);
    avx_get(c, vv, vvvv);
    avx_get_rm(state, c, I, next, width, operand);
    uint8_t *multiplier1 = (form == 0) ? destination : vvvv;
    uint8_t *multiplier2 = (form == 0) ? operand : (form == 1) ? destination : operand;
    uint8_t *addend = (form == 0) ? vvvv : (form == 1) ? operand : destination;
    int scalar = arithmetic && (op & 1);
    int count = scalar ? element_size : width;
    memcpy(output, destination, sizeof(output));
    for (int offset = 0; offset < count; offset += element_size) {
        int negate_product = 0;
        int negate_addend;
        if (alternating) {
            int subtract_on_odd = op & 1;
            int even = ((offset / element_size) & 1) == 0;
            negate_addend = subtract_on_odd ? !even : even;
        } else {
            int base = op & 0x0E; // 8=madd,A=msub,C=nmadd,E=nmsub
            negate_product = base == 0x0C || base == 0x0E;
            negate_addend = base == 0x0A || base == 0x0E;
        }
        if (dbl) {
            double x, y, z;
            memcpy(&x, multiplier1 + offset, 8);
            memcpy(&y, multiplier2 + offset, 8);
            memcpy(&z, addend + offset, 8);
            double result = fma_x86_f64(x, y, z, negate_product, negate_addend);
            memcpy(output + offset, &result, 8);
        } else {
            float x, y, z;
            memcpy(&x, multiplier1 + offset, 4);
            memcpy(&y, multiplier2 + offset, 4);
            memcpy(&z, addend + offset, 4);
            float result = fma_x86_f32(x, y, z, negate_product, negate_addend);
            memcpy(output + offset, &result, 4);
        }
    }
    avx_put(c, rd, output, scalar ? 16 : width);
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

// Keep the AES and carryless-multiply families together: they share the AES-NI
// opcode maps and are easier to audit here than among the general packed lanes.
static enum avx_dispatch_result avx_dispatch_crypto(const hl_x86_avx_state *state, struct cpu *c,
                                                    struct insn *instruction, uint64_t next, int map, int op,
                                                    int destination, int source, int width) {
    uint8_t left[64], right[64], output[64];
    if (map == 2 && op == 0xDB) { // vaesimc xmm, xmm/m128
        avx_get_rm(state, c, instruction, next, 16, right);
        memcpy(output, right, 16);
        aes_mixcolumns(output, 1);
        avx_put(c, destination, output, 16);
    } else if (map == 2 && op >= 0xDC && op <= 0xDF) { // vaesenc/last, vaesdec/last
        avx_get(c, source, left);
        avx_get_rm(state, c, instruction, next, 16, right);
        uint8_t transformed[16];
        int decrypt = op == 0xDE || op == 0xDF;
        aes_shiftrows(left, transformed, decrypt);
        aes_subbytes(transformed, decrypt ? k_aes_isbox : k_aes_sbox);
        if (op == 0xDC) aes_mixcolumns(transformed, 0);
        if (op == 0xDE) aes_mixcolumns(transformed, 1);
        for (int index = 0; index < 16; index++)
            output[index] = transformed[index] ^ right[index];
        avx_put(c, destination, output, 16);
    } else if (map == 3 && op == 0x44) { // vpclmulqdq
        avx_get(c, source, left);
        avx_get_rm(state, c, instruction, next, width, right);
        for (int lane = 0; lane < width; lane += 16) {
            uint64_t lhs, rhs;
            memcpy(&lhs, left + lane + 8 * (instruction->imm & 1), 8);
            memcpy(&rhs, right + lane + 8 * ((instruction->imm >> 4) & 1), 8);
// __int128: pre-C23 GNU/clang extension needed for the PCLMULQDQ carryless product; scope -Wpedantic.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wpedantic"
            unsigned __int128 product = 0;
            for (int bit = 0; bit < 64; bit++)
                if ((rhs >> bit) & 1) product ^= (unsigned __int128)lhs << bit;
#pragma GCC diagnostic pop
            memcpy(output + lane, &product, 16);
        }
        avx_put(c, destination, output, width);
    } else if (map == 3 && op == 0xDF) { // vaeskeygenassist
        avx_get_rm(state, c, instruction, next, 16, right);
        uint32_t words[4], result[4];
        memcpy(words, right, 16);
        uint32_t rcon = (uint32_t)(instruction->imm & 0xff);
        for (int index = 1; index <= 3; index += 2) {
            uint32_t word = words[index];
            uint32_t substituted = (uint32_t)k_aes_sbox[word & 0xff] | ((uint32_t)k_aes_sbox[(word >> 8) & 0xff] << 8) |
                                   ((uint32_t)k_aes_sbox[(word >> 16) & 0xff] << 16) |
                                   ((uint32_t)k_aes_sbox[(word >> 24) & 0xff] << 24);
            uint32_t rotated = (substituted >> 8) | (substituted << 24);
            result[index - 1] = substituted;
            result[index] = rotated ^ rcon;
        }
        memcpy(output, result, 16);
        avx_put(c, destination, output, 16);
    } else {
        return AVX_DISPATCH_UNMATCHED;
    }
    c->rip = next;
    return AVX_DISPATCH_HANDLED;
}

enum avx_dispatch_result avx_dispatch_special(const hl_x86_avx_state *state, struct cpu *cpu, struct insn *instruction,
                                              uint64_t next, int map, int op, int prefix, int destination,
                                              int first_register, int width) {
    enum avx_dispatch_result bmi =
        avx_dispatch_bmi(state, cpu, instruction, next, map, op, prefix, destination, first_register);
    if (bmi != AVX_DISPATCH_UNMATCHED) return bmi;
    if (avx_dispatch_fma(state, cpu, instruction, next, map, op, destination, first_register, width) ==
        AVX_DISPATCH_HANDLED)
        return AVX_DISPATCH_HANDLED;
    return avx_dispatch_crypto(state, cpu, instruction, next, map, op, destination, first_register, width);
}
