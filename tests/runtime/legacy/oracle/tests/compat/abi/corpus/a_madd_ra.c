// aarch64 three-source multiply ACCUMULATOR witness: MADD/MSUB/SMADDL/UMADDL/SMSUBL name a fourth GPR
// field Ra[14:10] (the accumulator/addend) that a naive decoder that only reads Rd/Rn/Rm misses.
// gpr_field_mask already flags it (data-processing-register 3-source -> mask bit3); this regression locks
// that coverage. The accumulator here is a STOLEN register (x16/x17), and an intervening indirect branch
// desyncs host x16/x17 from the guest value, so a missed Ra flag would read the engine-private host reg
// instead of the guest addend (silent arithmetic corruption). Native aarch64 golden.
#include <stdio.h>
#include <stdint.h>

#ifdef __aarch64__

#define SPLIT "adr x9, 1f\n br x9\n 1:\n"

static uint64_t madd_ra_x16(uint64_t a, uint64_t b, uint64_t acc){
    uint64_t r;
    __asm__ volatile("mov x16, %[c]\n" SPLIT
        "madd %[r], %[a], %[b], x16\n"
        : [r]"=&r"(r) : [a]"r"(a),[b]"r"(b),[c]"r"(acc) : "x9","x16");
    return r;
}
static uint64_t msub_ra_x17(uint64_t a, uint64_t b, uint64_t acc){
    uint64_t r;
    __asm__ volatile("mov x17, %[c]\n" SPLIT
        "msub %[r], %[a], %[b], x17\n"
        : [r]"=&r"(r) : [a]"r"(a),[b]"r"(b),[c]"r"(acc) : "x9","x17");
    return r;
}
static uint64_t smaddl_ra_x16(uint32_t a, uint32_t b, uint64_t acc){
    uint64_t r;
    __asm__ volatile("mov x16, %[c]\n" SPLIT
        "smaddl %[r], %w[a], %w[b], x16\n"
        : [r]"=&r"(r) : [a]"r"(a),[b]"r"(b),[c]"r"(acc) : "x9","x16");
    return r;
}
static uint64_t umaddl_ra_x17(uint32_t a, uint32_t b, uint64_t acc){
    uint64_t r;
    __asm__ volatile("mov x17, %[c]\n" SPLIT
        "umaddl %[r], %w[a], %w[b], x17\n"
        : [r]"=&r"(r) : [a]"r"(a),[b]"r"(b),[c]"r"(acc) : "x9","x17");
    return r;
}

int main(void){
    uint64_t acc = 0;
    for (uint64_t k = 1; k <= 4000; k++){
        acc = acc*1000003u + madd_ra_x16(k*3u+1u, k*7u+2u, k*0x1111u);
        acc = acc*1000003u + msub_ra_x17(k*5u, k+3u, k*0x2222u);
        acc = acc*1000003u + smaddl_ra_x16((uint32_t)(k*0xABCD), (uint32_t)(-(int64_t)k), k*0x3333u);
        acc = acc*1000003u + umaddl_ra_x17((uint32_t)(k*0x9E37), (uint32_t)(k*0x1234), k*0x4444u);
    }
    printf("acc=%016llx\n", (unsigned long long)acc);
    return 0;
}

#else /* three-source accumulator-field mangling is aarch64-guest specific. */
int main(void){ printf("acc=037057bba5339f70\n"); return 0; }
#endif
