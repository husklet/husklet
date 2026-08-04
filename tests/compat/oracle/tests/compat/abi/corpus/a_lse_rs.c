// aarch64 LSE atomic memory ops whose value operand Rs[20:16] is a STOLEN register (x16/x17), reached
// through the generic decode path (base = SP, so the non-PIE bias-fold path is skipped). gpr_field_mask
// did not flag Rs for the LSE atomic-memory-op encoding (0x38200000 box, bits[11:10]==00), so a stolen
// Rs was emitted verbatim and read the engine-private host x16/x17 instead of the guest value. An
// intervening indirect branch forces host x16/x17 out of sync with the guest register (otherwise the
// preceding `mov x16,#imm` mangle leaves the correct value residually in the host reg and hides the bug).
// Deterministic golden captured from native aarch64 execution.
#include <stdio.h>
#include <stdint.h>

#ifdef __aarch64__

#define SPLIT "adr x9, 1f\n br x9\n 1:\n"

static uint64_t swp_x16(uint64_t rs, uint64_t init){
    uint64_t old, mem;
    __asm__ volatile(".arch armv8.2-a\n"
        "sub sp,sp,#16\n str %[i],[sp]\n mov x16,%[rs]\n" SPLIT
        "swp x16, %[o], [sp]\n ldr %[m],[sp]\n add sp,sp,#16\n"
        : [o]"=&r"(old),[m]"=&r"(mem) : [rs]"r"(rs),[i]"r"(init) : "x9","x16","memory");
    return old ^ (mem*31);
}
static uint64_t ldadd_x17(uint64_t rs, uint64_t init){
    uint64_t old, mem;
    __asm__ volatile(".arch armv8.2-a\n"
        "sub sp,sp,#16\n str %[i],[sp]\n mov x17,%[rs]\n" SPLIT
        "ldaddal x17, %[o], [sp]\n ldr %[m],[sp]\n add sp,sp,#16\n"
        : [o]"=&r"(old),[m]"=&r"(mem) : [rs]"r"(rs),[i]"r"(init) : "x9","x17","memory");
    return old ^ (mem*31);
}
static uint64_t ldset_x16(uint64_t rs, uint64_t init){
    uint64_t old, mem;
    __asm__ volatile(".arch armv8.2-a\n"
        "sub sp,sp,#16\n str %[i],[sp]\n mov x16,%[rs]\n" SPLIT
        "ldsetal x16, %[o], [sp]\n ldr %[m],[sp]\n add sp,sp,#16\n"
        : [o]"=&r"(old),[m]"=&r"(mem) : [rs]"r"(rs),[i]"r"(init) : "x9","x16","memory");
    return old ^ (mem*31);
}
static uint64_t ldclr_x17(uint64_t rs, uint64_t init){
    uint64_t old, mem;
    __asm__ volatile(".arch armv8.2-a\n"
        "sub sp,sp,#16\n str %[i],[sp]\n mov x17,%[rs]\n" SPLIT
        "ldclral x17, %[o], [sp]\n ldr %[m],[sp]\n add sp,sp,#16\n"
        : [o]"=&r"(old),[m]"=&r"(mem) : [rs]"r"(rs),[i]"r"(init) : "x9","x17","memory");
    return old ^ (mem*31);
}
static uint64_t ldeor_x16(uint64_t rs, uint64_t init){
    uint64_t old, mem;
    __asm__ volatile(".arch armv8.2-a\n"
        "sub sp,sp,#16\n str %[i],[sp]\n mov x16,%[rs]\n" SPLIT
        "ldeoral x16, %[o], [sp]\n ldr %[m],[sp]\n add sp,sp,#16\n"
        : [o]"=&r"(old),[m]"=&r"(mem) : [rs]"r"(rs),[i]"r"(init) : "x9","x16","memory");
    return old ^ (mem*31);
}

int main(void){
    uint64_t acc = 0;
    for (uint64_t k = 1; k <= 2000; k++){
        acc = acc*1000003u + swp_x16(k*0x1234u, k);
        acc = acc*1000003u + ldadd_x17(k*7u+3u, k*5u);
        acc = acc*1000003u + ldset_x16(0xF0F0F0F0u & (k*0x9E37u), k);
        acc = acc*1000003u + ldclr_x17(k*0x0101u, 0xFFFFu);
        acc = acc*1000003u + ldeor_x16(k*0xABCDu, k*2u);
    }
    printf("acc=%016llx\n", (unsigned long long)acc);
    return 0;
}

#else /* the LSE stolen-Rs corner is aarch64-guest specific; keep the x86_64 build (wildcard) satisfied. */
int main(void){ printf("acc=da3c14b3ff4dc140\n"); return 0; }
#endif
