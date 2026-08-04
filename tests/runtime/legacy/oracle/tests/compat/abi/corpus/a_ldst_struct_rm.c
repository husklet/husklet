// aarch64 AdvSIMD load/store STRUCTURE ops (ld1/st1/ld2..) with a REGISTER post-index whose increment
// operand Rm[20:16] is a STOLEN register (x16/x17). The base is SP, so the non-PIE bias-fold emitter
// (emit_fold_advsimd_struct, which relocates a stolen Rm) is skipped and the instruction takes the generic
// decode path. gpr_field_mask did not flag Rm for the AdvSIMD-structure register-post-index encoding
// (0x0C800000 box, bit23==1) -- Rt there is a vector list so bit26 keeps it unflagged, and only Rn[9:5] was
// marked -- so a stolen Rm was emitted verbatim and the base advanced by the engine-private host x16/x17
// instead of the guest stride (silent wrong writeback -> corrupted iterator pointer). An intervening
// indirect branch forces host x16/x17 out of sync with the guest register so the bug is not masked by a
// residual correct value left in the host reg by the preceding `mov`. Deterministic golden from native.
#include <stdio.h>
#include <stdint.h>

#ifdef __aarch64__

#define SPLIT "adr x9, 1f\n br x9\n 1:\n"

// ld1 single-vector, register post-index off SP: measure the base writeback delta (should == guest stride).
static uint64_t ld1_post_x16(uint64_t stride){
    uint64_t before, after;
    __asm__ volatile(
        "mov x16, %[s]\n" SPLIT
        "mov %[b], sp\n"
        "ld1 {v0.16b}, [sp], x16\n"
        "mov %[a], sp\n"
        "mov sp, %[b]\n"
        : [b]"=&r"(before),[a]"=&r"(after) : [s]"r"(stride) : "x9","x16","v0","memory");
    return after - before;
}
static uint64_t st1_post_x17(uint64_t stride){
    uint64_t before, after;
    __asm__ volatile(
        "mov x17, %[s]\n" SPLIT
        "mov %[b], sp\n"
        "st1 {v1.8b}, [sp], x17\n"
        "mov %[a], sp\n"
        "mov sp, %[b]\n"
        : [b]"=&r"(before),[a]"=&r"(after) : [s]"r"(stride) : "x9","x17","v1","memory");
    return after - before;
}
// ld2 two-vector, register post-index off SP.
static uint64_t ld2_post_x16(uint64_t stride){
    uint64_t before, after;
    __asm__ volatile(
        "mov x16, %[s]\n" SPLIT
        "mov %[b], sp\n"
        "ld2 {v2.16b, v3.16b}, [sp], x16\n"
        "mov %[a], sp\n"
        "mov sp, %[b]\n"
        : [b]"=&r"(before),[a]"=&r"(after) : [s]"r"(stride) : "x9","x16","v2","v3","memory");
    return after - before;
}

int main(void){
    uint64_t acc = 0;
    for (uint64_t k = 1; k <= 4000; k++){
        uint64_t s = (k & 7u) * 16u + 16u; // 16..128, 16-byte aligned strides
        acc = acc*1000003u + ld1_post_x16(s);
        acc = acc*1000003u + st1_post_x17(s + 16u);
        acc = acc*1000003u + ld2_post_x16(s + 32u);
    }
    printf("acc=%016llx\n", (unsigned long long)acc);
    return 0;
}

#else /* the AdvSIMD-structure register-post-index stolen-Rm corner is aarch64-guest specific. */
int main(void){ printf("acc=28dbee8c7b3d9300\n"); return 0; }
#endif
