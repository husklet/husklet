// SHLD/SHRD flag differential (M item): the engine now materializes CF (last bit shifted out) and PF in
// addition to SF/ZF. Capture RFLAGS right after each shld/shrd and diff (CF|PF|ZF|SF = 0xC5) vs the qemu
// oracle. OF/AF are undefined for these ops (except OF at count==1) and deliberately NOT checked.
#include <stdint.h>
#include <stdio.h>

#define MASK 0xC5u // CF(0) | PF(2) | ZF(6) | SF(7)

static unsigned long fl_shld64_imm(unsigned long d, unsigned long s, int imm) {
    unsigned long fl;
    switch (imm) {
    case 4:  __asm__ volatile("shldq $4,%2,%1\n\tpushfq\n\tpopq %0"  : "=r"(fl), "+r"(d) : "r"(s) : "cc"); break;
    default: __asm__ volatile("shldq $31,%2,%1\n\tpushfq\n\tpopq %0" : "=r"(fl), "+r"(d) : "r"(s) : "cc"); break;
    }
    (void)d;
    return fl & MASK;
}
static unsigned long fl_shrd64_imm(unsigned long d, unsigned long s, int imm) {
    unsigned long fl;
    switch (imm) {
    case 4:  __asm__ volatile("shrdq $4,%2,%1\n\tpushfq\n\tpopq %0"  : "=r"(fl), "+r"(d) : "r"(s) : "cc"); break;
    default: __asm__ volatile("shrdq $1,%2,%1\n\tpushfq\n\tpopq %0"  : "=r"(fl), "+r"(d) : "r"(s) : "cc"); break;
    }
    (void)d;
    return fl & MASK;
}
static unsigned long fl_shld32_imm(unsigned d, unsigned s) {
    unsigned long fl;
    __asm__ volatile("shldl $12,%2,%1\n\tpushfq\n\tpopq %0" : "=r"(fl), "+r"(d) : "r"(s) : "cc");
    (void)d;
    return fl & MASK;
}
static unsigned long fl_shrd32_imm(unsigned d, unsigned s) {
    unsigned long fl;
    __asm__ volatile("shrdl $7,%2,%1\n\tpushfq\n\tpopq %0" : "=r"(fl), "+r"(d) : "r"(s) : "cc");
    (void)d;
    return fl & MASK;
}
// by CL. count in cl; result in d. Capture CF|PF|ZF|SF.
static unsigned long fl_shld64_cl(unsigned long d, unsigned long s, unsigned cl) {
    unsigned long fl;
    __asm__ volatile("shldq %%cl,%2,%1\n\tpushfq\n\tpopq %0" : "=r"(fl), "+r"(d) : "r"(s), "c"(cl) : "cc");
    (void)d;
    return fl & MASK;
}
static unsigned long fl_shrd32_cl(unsigned d, unsigned s, unsigned cl) {
    unsigned long fl;
    __asm__ volatile("shrdl %%cl,%2,%1\n\tpushfq\n\tpopq %0" : "=r"(fl), "+r"(d) : "r"(s), "c"(cl) : "cc");
    (void)d;
    return fl & MASK;
}
// count==0 with CL: ALL flags preserved. Pre-set CF via stc/clc, then shld %cl (cl=0), read CF back.
static int cf_shld_cl0(unsigned long d, unsigned long s, int precf) {
    unsigned long fl;
    if (precf)
        __asm__ volatile("stc\n\tshldq %%cl,%2,%1\n\tpushfq\n\tpopq %0" : "=r"(fl), "+r"(d) : "r"(s), "c"(0u) : "cc");
    else
        __asm__ volatile("clc\n\tshldq %%cl,%2,%1\n\tpushfq\n\tpopq %0" : "=r"(fl), "+r"(d) : "r"(s), "c"(0u) : "cc");
    (void)d;
    return (int)(fl & 1u); // CF preserved from stc/clc?
}

int main(void) {
    printf("shld64 imm4  %02lx\n", fl_shld64_imm(0x8000000000000001ul, 0xF0F0F0F0F0F0F0F0ul, 4));
    printf("shld64 imm31 %02lx\n", fl_shld64_imm(0x0000000100000000ul, 0x1ul, 31));
    printf("shrd64 imm4  %02lx\n", fl_shrd64_imm(0x1000000000000001ul, 0xAul, 4));
    printf("shrd64 imm1  %02lx\n", fl_shrd64_imm(0x1ul, 0x0ul, 1));
    printf("shld32 imm12 %02lx\n", fl_shld32_imm(0x80100001u, 0xABCDEF12u));
    printf("shrd32 imm7  %02lx\n", fl_shrd32_imm(0x000000C0u, 0xFF00FF00u));
    printf("shld64 cl1   %02lx\n", fl_shld64_cl(0x8000000000000000ul, 0x1ul, 1));
    printf("shld64 cl5   %02lx\n", fl_shld64_cl(0x0800000000000000ul, 0x0ul, 5));
    printf("shld64 cl63  %02lx\n", fl_shld64_cl(0x1ul, 0x8000000000000000ul, 63));
    printf("shrd32 cl3   %02lx\n", fl_shrd32_cl(0x00000004u, 0x0u, 3));
    printf("shrd32 cl31  %02lx\n", fl_shrd32_cl(0x80000000u, 0x0u, 31));
    printf("shld cl0 stc %d\n", cf_shld_cl0(0x1234ul, 0x5678ul, 1)); // CF must stay 1
    printf("shld cl0 clc %d\n", cf_shld_cl0(0x1234ul, 0x5678ul, 0)); // CF must stay 0
    return 0;
}
